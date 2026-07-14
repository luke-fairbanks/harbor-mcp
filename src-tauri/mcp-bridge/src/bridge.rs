use crate::descriptor::Descriptor;
use crate::runtime::{NativeRuntime, PostResult, Runtime, RuntimeError};
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

const BACKEND_ERROR: &str = "Harbor is unavailable; the bridge will reconnect automatically.";

/// Bounds recovery work so a disconnected client receives a useful JSON-RPC
/// error while the stdio process remains available for its next request.
#[derive(Clone, Copy, Debug)]
pub struct BridgeTiming {
    pub startup_wait: Duration,
    pub poll_interval: Duration,
    pub start_cooldown: Duration,
}

impl Default for BridgeTiming {
    fn default() -> Self {
        Self {
            startup_wait: Duration::from_secs(10),
            poll_interval: Duration::from_millis(200),
            start_cooldown: Duration::from_secs(2),
        }
    }
}

/// A persistent, sequential JSON-RPC dispatcher with a single stdout writer.
pub struct Bridge {
    core: BridgeCore<NativeRuntime>,
}

impl Bridge {
    pub fn new(runtime: NativeRuntime) -> Self {
        Self {
            core: BridgeCore::new(runtime, BridgeTiming::default()),
        }
    }

    pub fn with_timing(runtime: NativeRuntime, timing: BridgeTiming) -> Self {
        Self {
            core: BridgeCore::new(runtime, timing),
        }
    }

    /// Process one newline-delimited JSON-RPC message. Every returned string is
    /// compact JSON suitable for stdout; operational failures never escape as
    /// unstructured output.
    pub fn process_line(&mut self, line: &str) -> Vec<String> {
        self.core.process_line(line)
    }
}

struct BridgeCore<R: Runtime> {
    runtime: R,
    timing: BridgeTiming,
    active_descriptor: Option<Descriptor>,
    session: Session,
    last_start: Option<Instant>,
}

#[derive(Default)]
struct Session {
    initialize_request: Option<Value>,
    protocol_version: Option<String>,
    initialized: bool,
}

struct ParsedMessage {
    value: Value,
    id: Option<Value>,
    method: Option<String>,
    expects_response: bool,
}

enum ParseFailure {
    Parse,
    Invalid,
}

impl<R: Runtime> BridgeCore<R> {
    fn new(runtime: R, timing: BridgeTiming) -> Self {
        Self {
            runtime,
            timing,
            active_descriptor: None,
            session: Session::default(),
            last_start: None,
        }
    }

    fn process_line(&mut self, line: &str) -> Vec<String> {
        let parsed = match parse_message(line) {
            Ok(parsed) => parsed,
            Err(ParseFailure::Parse) => return vec![serialize(parse_error())],
            Err(ParseFailure::Invalid) => return vec![serialize(invalid_request())],
        };
        let mut outputs = Vec::new();
        if parsed.method.as_deref() == Some("ping") {
            if parsed.expects_response {
                outputs.push(serialize(json!({
                    "jsonrpc": "2.0",
                    "id": parsed.id,
                    "result": {}
                })));
            }
            return outputs;
        }
        let is_initialize = parsed.method.as_deref() == Some("initialize");
        let is_initialized = parsed.method.as_deref() == Some("notifications/initialized");

        let descriptor = match self.ensure_ready() {
            Ok(descriptor) => descriptor,
            Err(_) => {
                if is_initialized && self.session.initialize_request.is_some() {
                    self.session.initialized = true;
                }
                if parsed.expects_response {
                    outputs.push(serialize(backend_error(parsed.id.as_ref())));
                }
                return outputs;
            }
        };

        if self.active_descriptor.as_ref() != Some(&descriptor) {
            let activation = if is_initialize {
                self.active_descriptor = Some(descriptor.clone());
                Ok(())
            } else {
                self.activate_backend(&descriptor, &mut outputs)
            };
            if activation.is_err() {
                if is_initialized && self.session.initialize_request.is_some() {
                    self.session.initialized = true;
                }
                if parsed.expects_response {
                    outputs.push(serialize(backend_error(parsed.id.as_ref())));
                }
                return outputs;
            }
        }

        let protocol = if is_initialize {
            None
        } else {
            self.session.protocol_version.clone()
        };
        let result = self.post_with_one_retry(
            &descriptor,
            &parsed.value,
            protocol.as_deref(),
            is_initialize,
            &mut outputs,
        );

        if is_initialized && self.session.initialize_request.is_some() {
            // Remember the client's state even if Harbor disappeared between
            // validation and POST. A future backend can then be restored.
            self.session.initialized = true;
        }

        match result {
            Ok((PostResult::Json(mut response), final_descriptor)) => {
                self.active_descriptor = Some(final_descriptor);
                if is_initialize && parsed.expects_response {
                    augment_initialize_capabilities(&mut response);
                    self.session.initialize_request = Some(parsed.value.clone());
                    self.session.protocol_version = negotiated_protocol(&response)
                        .or_else(|| requested_protocol(&parsed.value));
                    self.session.initialized = false;
                }
                if parsed.expects_response {
                    outputs.push(serialize(response));
                }
            }
            Ok((PostResult::Accepted, final_descriptor)) => {
                self.active_descriptor = Some(final_descriptor);
                if parsed.expects_response {
                    outputs.push(serialize(backend_error(parsed.id.as_ref())));
                }
            }
            Err(_) => {
                if parsed.expects_response {
                    outputs.push(serialize(backend_error(parsed.id.as_ref())));
                }
            }
        }
        outputs
    }

    fn ensure_ready(&mut self) -> Result<Descriptor, RuntimeError> {
        let first = self.runtime.read_descriptor();
        if let Ok(descriptor) = &first {
            if self.runtime.validate_backend(descriptor).is_ok() {
                return Ok(descriptor.clone());
            }
        }

        let startup_descriptor = first.ok();
        self.start_if_due(startup_descriptor.as_ref());
        self.wait_for_ready(None, false)
    }

    fn start_if_due(&mut self, descriptor: Option<&Descriptor>) {
        let now = self.runtime.now();
        let may_start = self
            .last_start
            .is_none_or(|last| now.saturating_duration_since(last) >= self.timing.start_cooldown);
        if may_start {
            // Record before spawning: the sequential dispatcher then provides
            // singleflight behavior even when process creation itself fails.
            self.last_start = Some(now);
            let _ = self.runtime.start_harbor(descriptor);
        }
    }

    fn wait_for_ready(
        &mut self,
        previous: Option<&Descriptor>,
        require_change: bool,
    ) -> Result<Descriptor, RuntimeError> {
        let deadline = self.runtime.now() + self.timing.startup_wait;
        loop {
            if let Ok(candidate) = self.runtime.read_descriptor() {
                let changed = previous != Some(&candidate);
                if (!require_change || changed) && self.runtime.validate_backend(&candidate).is_ok()
                {
                    return Ok(candidate);
                }
            }
            if self.runtime.now() >= deadline {
                return Err(RuntimeError::Unavailable);
            }
            self.runtime.sleep(self.timing.poll_interval);
        }
    }

    fn activate_backend(
        &mut self,
        descriptor: &Descriptor,
        outputs: &mut Vec<String>,
    ) -> Result<(), RuntimeError> {
        let previous = self.active_descriptor.clone();
        if let Some(initialize) = self.session.initialize_request.as_ref() {
            match self.runtime.post(descriptor, initialize, None)? {
                PostResult::Json(response) if response.get("result").is_some() => {
                    // The client negotiated this logical session with the old
                    // backend. A replacement backend must accept that same MCP
                    // protocol; silently switching versions would leave the
                    // two ends of the stdio session disagreeing.
                    if self
                        .session
                        .protocol_version
                        .as_deref()
                        .is_some_and(|expected| {
                            negotiated_protocol(&response).as_deref() != Some(expected)
                        })
                    {
                        return Err(RuntimeError::Rejected);
                    }
                }
                _ => return Err(RuntimeError::Rejected),
            }
            if self.session.initialized {
                let initialized = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                });
                if !matches!(
                    self.runtime.post(
                        descriptor,
                        &initialized,
                        self.session.protocol_version.as_deref()
                    )?,
                    PostResult::Accepted
                ) {
                    return Err(RuntimeError::Rejected);
                }
            }
        }
        self.active_descriptor = Some(descriptor.clone());

        if previous.is_some()
            && previous.as_ref() != Some(descriptor)
            && self.session.initialize_request.is_some()
            && self.session.initialized
        {
            outputs.push(serialize(json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed"
            })));
        }
        Ok(())
    }

    fn post_with_one_retry(
        &mut self,
        descriptor: &Descriptor,
        message: &Value,
        protocol: Option<&str>,
        current_is_initialize: bool,
        outputs: &mut Vec<String>,
    ) -> Result<(PostResult, Descriptor), RuntimeError> {
        match self.runtime.post(descriptor, message, protocol) {
            Ok(result) => Ok((result, descriptor.clone())),
            Err(RuntimeError::PreSend) => {
                // Listener verification or connection establishment failed, so
                // the backend did not see the operation. Starting Harbor and
                // retrying once is safe.
                let current = self.runtime.read_descriptor().ok();
                self.start_if_due(current.as_ref().or(Some(descriptor)));
                let recovered = self.wait_for_ready(None, false)?;
                self.prepare_recovered_backend(&recovered, current_is_initialize, outputs)?;
                let result = self.runtime.post(
                    &recovered,
                    message,
                    if current_is_initialize {
                        None
                    } else {
                        self.session.protocol_version.as_deref()
                    },
                )?;
                Ok((result, recovered))
            }
            Err(RuntimeError::Unauthorized) => {
                // A 401 is known to occur before Harbor dispatches JSON-RPC,
                // but retry only after the protected descriptor truly rotates.
                let recovered = self.wait_for_ready(Some(descriptor), true)?;
                self.prepare_recovered_backend(&recovered, current_is_initialize, outputs)?;
                let result = self.runtime.post(
                    &recovered,
                    message,
                    if current_is_initialize {
                        None
                    } else {
                        self.session.protocol_version.as_deref()
                    },
                )?;
                Ok((result, recovered))
            }
            // Rejected HTTP responses, timeouts, and response-body failures
            // may follow an already-executed tools/call and are never replayed.
            Err(error) => Err(error),
        }
    }

    fn prepare_recovered_backend(
        &mut self,
        recovered: &Descriptor,
        current_is_initialize: bool,
        outputs: &mut Vec<String>,
    ) -> Result<(), RuntimeError> {
        if self.active_descriptor.as_ref() == Some(recovered) {
            return Ok(());
        }
        if current_is_initialize {
            self.active_descriptor = Some(recovered.clone());
            Ok(())
        } else {
            self.activate_backend(recovered, outputs)
        }
    }
}

fn parse_message(line: &str) -> Result<ParsedMessage, ParseFailure> {
    let value: Value = serde_json::from_str(line).map_err(|_| ParseFailure::Parse)?;
    let object = value.as_object().ok_or(ParseFailure::Invalid)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ParseFailure::Invalid);
    }

    let id = object.get("id").cloned();
    if id
        .as_ref()
        .is_some_and(|id| !(id.is_string() || id.is_number() || id.is_null()))
    {
        return Err(ParseFailure::Invalid);
    }
    let method = match object.get("method") {
        Some(method) => Some(method.as_str().ok_or(ParseFailure::Invalid)?.to_owned()),
        None => None,
    };
    if method.as_ref().is_some_and(String::is_empty) {
        return Err(ParseFailure::Invalid);
    }
    if object
        .get("params")
        .is_some_and(|params| !params.is_object() && !params.is_array())
    {
        return Err(ParseFailure::Invalid);
    }

    let is_method_message = method.is_some();
    let is_response = method.is_none()
        && id.is_some()
        && (object.contains_key("result") ^ object.contains_key("error"));
    if !is_method_message && !is_response {
        return Err(ParseFailure::Invalid);
    }

    Ok(ParsedMessage {
        value,
        id: id.clone(),
        method,
        expects_response: is_method_message && id.is_some(),
    })
}

fn parse_error() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32700, "message": "Parse error"}
    })
}

fn invalid_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32600, "message": "Invalid Request"}
    })
}

fn backend_error(id: Option<&Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": {"code": -32000, "message": BACKEND_ERROR}
    })
}

fn serialize(value: Value) -> String {
    serde_json::to_string(&value).expect("JSON values always serialize")
}

fn requested_protocol(request: &Value) -> Option<String> {
    request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| valid_protocol(version))
        .map(ToOwned::to_owned)
}

fn negotiated_protocol(response: &Value) -> Option<String> {
    response
        .pointer("/result/protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| valid_protocol(version))
        .map(ToOwned::to_owned)
}

fn valid_protocol(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

fn augment_initialize_capabilities(response: &mut Value) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    let capabilities = object_entry(result, "capabilities");
    let tools = object_entry(capabilities, "tools");
    tools.insert("listChanged".to_string(), Value::Bool(true));
}

fn object_entry<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !object.get(key).is_some_and(Value::is_object) {
        object.insert(key.to_string(), Value::Object(Map::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object inserted above")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::native_runtime;
    use std::collections::VecDeque;
    use std::fs::{File, Permissions};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const PROTOCOL: &str = "2025-11-25";

    #[derive(Debug, Clone)]
    struct RequestRecord {
        method: String,
        authorization: String,
        protocol: Option<String>,
    }

    struct MockServer {
        port: u16,
        records: Arc<Mutex<Vec<RequestRecord>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn start(label: &'static str, token: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let records = Arc::new(Mutex::new(Vec::new()));
            let thread_records = records.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let thread = thread::spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_connection(stream, label, token, &thread_records),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                port,
                records,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn handle_connection(
        mut stream: TcpStream,
        label: &str,
        token: &str,
        records: &Arc<Mutex<Vec<RequestRecord>>>,
    ) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0usize;
        let mut authorization = String::new();
        let mut protocol = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap(),
                "authorization" => authorization = value.trim().to_string(),
                "mcp-protocol-version" => protocol = Some(value.trim().to_string()),
                _ => {}
            }
        }
        let expected_auth = format!("Bearer {token}");
        if authorization != expected_auth {
            write_response(&mut stream, 401, "text/plain", b"unauthorized");
            return;
        }
        if request_line.starts_with("GET /health ") {
            write_response(&mut stream, 200, "text/plain", b"Harbor MCP OK");
            return;
        }
        if !request_line.starts_with("POST /mcp ") {
            write_response(&mut stream, 404, "text/plain", b"missing");
            return;
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        let message: Value = serde_json::from_slice(&body).unwrap();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("response")
            .to_string();
        records.lock().unwrap().push(RequestRecord {
            method: method.clone(),
            authorization,
            protocol,
        });
        let Some(id) = message.get("id") else {
            write_response(&mut stream, 202, "text/plain", b"");
            return;
        };
        let response = if method == "initialize" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": label, "version": "test"}
                }
            })
        } else {
            json!({"jsonrpc":"2.0","id":id,"result":{"server":label}})
        };
        write_response(
            &mut stream,
            200,
            "application/json",
            serde_json::to_string(&response).unwrap().as_bytes(),
        );
    }

    fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            401 => "Unauthorized",
            _ => "Not Found",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
    }

    fn write_descriptor(path: &Path, port: u16, token: &str, instance: &str) {
        let tmp = path.with_extension("tmp");
        let descriptor = json!({
            "schemaVersion": 1,
            "instanceId": instance,
            "pid": 42,
            "processStartedAt": "test",
            "port": port,
            "token": token,
            "appExecutable": "/Applications/Harbor.app/Contents/MacOS/Harbor"
        });
        let mut file = File::create(&tmp).unwrap();
        file.write_all(serde_json::to_string(&descriptor).unwrap().as_bytes())
            .unwrap();
        file.set_permissions(Permissions::from_mode(0o600)).unwrap();
        file.sync_all().unwrap();
        std::fs::rename(tmp, path).unwrap();
    }

    fn fast_timing() -> BridgeTiming {
        BridgeTiming {
            startup_wait: Duration::from_millis(150),
            poll_interval: Duration::from_millis(5),
            start_cooldown: Duration::from_secs(1),
        }
    }

    fn initialize(id: Value) -> String {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name":"test","version":"1"}
            }
        }))
        .unwrap()
    }

    fn list_tools(id: Value) -> String {
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}}))
            .unwrap()
    }

    #[test]
    fn one_bridge_follows_atomic_backend_rotation_and_replays_session() {
        let server_a = MockServer::start("A", "token-aaaaaaaaaa");
        let server_b = MockServer::start("B", "token-bbbbbbbbbb");
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), Permissions::from_mode(0o700)).unwrap();
        let settings = dir.path().join("mcp.json");
        write_descriptor(&settings, server_a.port, "token-aaaaaaaaaa", "A");
        let starts = Arc::new(AtomicUsize::new(0));
        let runtime = native_runtime(settings.clone(), starts.clone());
        let mut bridge = BridgeCore::new(runtime, fast_timing());

        let init_output = bridge.process_line(&initialize(json!("init-id")));
        assert_eq!(init_output.len(), 1);
        let init: Value = serde_json::from_str(&init_output[0]).unwrap();
        assert_eq!(init["id"], "init-id");
        assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
        assert!(bridge
            .process_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_empty());
        let before = bridge.process_line(&list_tools(json!(1)));
        assert_eq!(
            serde_json::from_str::<Value>(&before[0]).unwrap()["result"]["server"],
            "A"
        );

        // Atomic replacement models Harbor's token, process, and port rotation
        // while this exact BridgeCore (and therefore stdio session) stays live.
        write_descriptor(&settings, server_b.port, "token-bbbbbbbbbb", "B");
        let after = bridge.process_line(&list_tools(json!("after")));
        assert_eq!(after.len(), 2);
        let changed: Value = serde_json::from_str(&after[0]).unwrap();
        assert_eq!(changed["method"], "notifications/tools/list_changed");
        let response: Value = serde_json::from_str(&after[1]).unwrap();
        assert_eq!(response["id"], "after");
        assert_eq!(response["result"]["server"], "B");
        assert_eq!(starts.load(Ordering::SeqCst), 0);

        let records = server_b.records.lock().unwrap().clone();
        assert_eq!(
            records
                .iter()
                .map(|record| record.method.as_str())
                .collect::<Vec<_>>(),
            ["initialize", "notifications/initialized", "tools/list"]
        );
        assert!(records
            .iter()
            .all(|record| record.authorization == "Bearer token-bbbbbbbbbb"));
        assert_eq!(records[0].protocol, None);
        assert_eq!(records[1].protocol.as_deref(), Some(PROTOCOL));
        assert_eq!(records[2].protocol.as_deref(), Some(PROTOCOL));
        assert!(after.iter().all(|line| !line.contains("token-")));
    }

    #[test]
    fn malformed_descriptor_is_sanitized_and_bridge_recovers_later() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), Permissions::from_mode(0o700)).unwrap();
        let settings = dir.path().join("mcp.json");
        let secret = "very-secret-token-material";
        std::fs::write(&settings, format!(r#"{{"port":0,"token":"{secret}"}}"#)).unwrap();
        std::fs::set_permissions(&settings, Permissions::from_mode(0o600)).unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let runtime = native_runtime(settings.clone(), starts.clone());
        let mut bridge = BridgeCore::new(
            runtime,
            BridgeTiming {
                startup_wait: Duration::ZERO,
                poll_interval: Duration::ZERO,
                start_cooldown: Duration::from_secs(30),
            },
        );
        let failed = bridge.process_line(&list_tools(json!(7)));
        assert_eq!(failed.len(), 1);
        assert!(!failed[0].contains(secret));
        assert_eq!(
            serde_json::from_str::<Value>(&failed[0]).unwrap()["error"]["code"],
            -32000
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        let server = MockServer::start("later", "token-cccccccccc");
        write_descriptor(&settings, server.port, "token-cccccccccc", "later");
        let recovered = bridge.process_line(&list_tools(json!(8)));
        assert_eq!(
            serde_json::from_str::<Value>(&recovered[0]).unwrap()["result"]["server"],
            "later"
        );
    }

    #[derive(Clone)]
    struct FakeRuntime {
        descriptor: Descriptor,
        outcomes: Arc<Mutex<VecDeque<Result<PostResult, RuntimeError>>>>,
        posts: Arc<AtomicUsize>,
    }

    impl Runtime for FakeRuntime {
        fn read_descriptor(&self) -> Result<Descriptor, RuntimeError> {
            Ok(self.descriptor.clone())
        }
        fn validate_backend(&self, _descriptor: &Descriptor) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn post(
            &self,
            _descriptor: &Descriptor,
            _message: &Value,
            _protocol_version: Option<&str>,
        ) -> Result<PostResult, RuntimeError> {
            self.posts.fetch_add(1, Ordering::SeqCst);
            self.outcomes.lock().unwrap().pop_front().unwrap()
        }
        fn start_harbor(&self, _descriptor: Option<&Descriptor>) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn now(&self) -> Instant {
            Instant::now()
        }
        fn sleep(&self, _duration: Duration) {}
    }

    fn fake_descriptor() -> Descriptor {
        Descriptor {
            schema_version: 1,
            instance_id: "fake".into(),
            pid: 1,
            process_started_at: None,
            port: 1234,
            token: "0123456789abcdef".into(),
            app_executable: None,
        }
    }

    #[test]
    fn ambiguous_post_failure_is_not_replayed() {
        let posts = Arc::new(AtomicUsize::new(0));
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            Err(RuntimeError::Ambiguous),
            Ok(PostResult::Json(json!({
                "jsonrpc":"2.0","id":2,"result":{"ok":true}
            }))),
        ])));
        let runtime = FakeRuntime {
            descriptor: fake_descriptor(),
            outcomes,
            posts: posts.clone(),
        };
        let mut bridge = BridgeCore::new(runtime, fast_timing());
        let first = bridge.process_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stop_app","arguments":{}}}"#,
        );
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert_eq!(serde_json::from_str::<Value>(&first[0]).unwrap()["id"], 1);

        let second = bridge.process_line(&list_tools(json!(2)));
        assert_eq!(posts.load(Ordering::SeqCst), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&second[0]).unwrap()["result"]["ok"],
            true
        );
    }

    #[test]
    fn backend_rotation_rejects_a_changed_protocol_negotiation() {
        let posts = Arc::new(AtomicUsize::new(0));
        let outcomes = Arc::new(Mutex::new(VecDeque::from([Ok(PostResult::Json(json!({
            "jsonrpc":"2.0",
            "id":"init-id",
            "result": {
                "protocolVersion":"2024-11-05",
                "capabilities":{},
                "serverInfo":{"name":"new","version":"1"}
            }
        })))])));
        let mut replacement = fake_descriptor();
        replacement.instance_id = "replacement".into();
        let runtime = FakeRuntime {
            descriptor: replacement,
            outcomes,
            posts: posts.clone(),
        };
        let mut bridge = BridgeCore::new(runtime, fast_timing());
        bridge.active_descriptor = Some(fake_descriptor());
        bridge.session.initialize_request =
            Some(serde_json::from_str(&initialize(json!("init-id"))).unwrap());
        bridge.session.protocol_version = Some(PROTOCOL.into());
        bridge.session.initialized = true;

        let output = bridge.process_line(&list_tools(json!(3)));
        assert_eq!(
            posts.load(Ordering::SeqCst),
            1,
            "current request was not sent"
        );
        assert_eq!(output.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&output[0]).unwrap()["error"]["code"],
            -32000
        );
    }

    struct RevalidationRuntime {
        descriptor: Descriptor,
        validations: Arc<AtomicUsize>,
        posts: Arc<AtomicUsize>,
    }

    impl Runtime for RevalidationRuntime {
        fn read_descriptor(&self) -> Result<Descriptor, RuntimeError> {
            Ok(self.descriptor.clone())
        }
        fn validate_backend(&self, _descriptor: &Descriptor) -> Result<(), RuntimeError> {
            if self.validations.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err(RuntimeError::Unavailable)
            }
        }
        fn post(
            &self,
            _descriptor: &Descriptor,
            message: &Value,
            _protocol_version: Option<&str>,
        ) -> Result<PostResult, RuntimeError> {
            self.posts.fetch_add(1, Ordering::SeqCst);
            Ok(PostResult::Json(json!({
                "jsonrpc":"2.0",
                "id":message["id"].clone(),
                "result":{}
            })))
        }
        fn start_harbor(&self, _descriptor: Option<&Descriptor>) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn now(&self) -> Instant {
            Instant::now()
        }
        fn sleep(&self, _duration: Duration) {}
    }

    #[test]
    fn listener_ownership_and_health_are_revalidated_before_every_post() {
        let validations = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let runtime = RevalidationRuntime {
            descriptor: fake_descriptor(),
            validations: validations.clone(),
            posts: posts.clone(),
        };
        let mut bridge = BridgeCore::new(
            runtime,
            BridgeTiming {
                startup_wait: Duration::ZERO,
                poll_interval: Duration::ZERO,
                start_cooldown: Duration::from_secs(1),
            },
        );
        assert_eq!(bridge.process_line(&list_tools(json!(1))).len(), 1);
        assert_eq!(posts.load(Ordering::SeqCst), 1);

        // The second validation models the original Harbor listener exiting
        // and a foreign process taking its old port. No bearer-bearing POST is
        // allowed after ownership/health validation fails.
        let second = bridge.process_line(&list_tools(json!(2)));
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert!(validations.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            serde_json::from_str::<Value>(&second[0]).unwrap()["error"]["code"],
            -32000
        );
    }

    #[test]
    fn notifications_never_create_stdout_and_parse_errors_are_json_rpc() {
        let posts = Arc::new(AtomicUsize::new(0));
        let runtime = FakeRuntime {
            descriptor: fake_descriptor(),
            outcomes: Arc::new(Mutex::new(VecDeque::from([Ok(PostResult::Accepted)]))),
            posts,
        };
        let mut bridge = BridgeCore::new(runtime, fast_timing());
        assert!(bridge
            .process_line(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#
            )
            .is_empty());
        let parse = bridge.process_line("not json");
        let parsed: Value = serde_json::from_str(&parse[0]).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
        let batch = bridge.process_line("[]");
        assert_eq!(
            serde_json::from_str::<Value>(&batch[0]).unwrap()["error"]["code"],
            -32600
        );
    }

    #[test]
    fn ping_is_local_and_never_starts_or_contacts_harbor() {
        let posts = Arc::new(AtomicUsize::new(0));
        let runtime = FakeRuntime {
            descriptor: fake_descriptor(),
            outcomes: Arc::new(Mutex::new(VecDeque::new())),
            posts: posts.clone(),
        };
        let mut bridge = BridgeCore::new(runtime, fast_timing());
        let output = bridge.process_line(r#"{"jsonrpc":"2.0","id":"keepalive","method":"ping"}"#);
        assert_eq!(output.len(), 1);
        let response: Value = serde_json::from_str(&output[0]).unwrap();
        assert_eq!(response["id"], "keepalive");
        assert_eq!(response["result"], json!({}));
        assert_eq!(posts.load(Ordering::SeqCst), 0);
    }
}
