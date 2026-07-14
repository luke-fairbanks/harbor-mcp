#![cfg(target_os = "macos")]

use serde_json::{json, Value};
use std::fs::{File, Permissions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-11-25";

#[derive(Clone)]
struct Record {
    method: String,
    protocol: Option<String>,
}

struct Server {
    port: u16,
    records: Arc<Mutex<Vec<Record>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
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
                    Ok((stream, _)) => serve(stream, label, token, &thread_records),
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

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, label: &str, token: &str, records: &Arc<Mutex<Vec<Record>>>) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
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
    if authorization != format!("Bearer {token}") {
        respond(&mut stream, 401, b"unauthorized");
        return;
    }
    if request_line.starts_with("GET /health ") {
        respond(&mut stream, 200, b"Harbor MCP OK");
        return;
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    let method = request["method"].as_str().unwrap().to_string();
    records.lock().unwrap().push(Record {
        method: method.clone(),
        protocol,
    });
    let Some(id) = request.get("id") else {
        respond(&mut stream, 202, b"");
        return;
    };
    let response = if method == "initialize" {
        json!({
            "jsonrpc":"2.0",
            "id":id,
            "result":{
                "protocolVersion":PROTOCOL,
                "capabilities":{"tools":{}},
                "serverInfo":{"name":label,"version":"test"}
            }
        })
    } else {
        json!({"jsonrpc":"2.0","id":id,"result":{"server":label}})
    };
    respond(
        &mut stream,
        200,
        serde_json::to_string(&response).unwrap().as_bytes(),
    );
}

fn respond(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        _ => "Unauthorized",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

fn descriptor(path: &Path, port: u16, token: &str, instance: &str) {
    let temporary = path.with_extension("tmp");
    let pid = std::process::id();
    let pid_string = pid.to_string();
    let started = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid_string])
        .output()
        .unwrap();
    assert!(started.status.success());
    let started = String::from_utf8(started.stdout).unwrap();
    let value = json!({
        "schemaVersion":1,
        "instanceId":instance,
        "pid":pid,
        "processStartedAt":started.trim(),
        "port":port,
        "token":token
    });
    let mut file = File::create(&temporary).unwrap();
    file.write_all(serde_json::to_string(&value).unwrap().as_bytes())
        .unwrap();
    file.set_permissions(Permissions::from_mode(0o600)).unwrap();
    file.sync_all().unwrap();
    std::fs::rename(temporary, path).unwrap();
}

fn send(stdin: &mut impl Write, message: Value) {
    writeln!(stdin, "{}", serde_json::to_string(&message).unwrap()).unwrap();
    stdin.flush().unwrap();
}

fn receive(stdout: &mut impl BufRead) -> Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "bridge stdout closed unexpectedly");
    serde_json::from_str(&line).unwrap()
}

#[test]
fn child_stdio_session_survives_real_descriptor_rotation() {
    let server_a = Server::start("A", "stdio-token-aaaaaaaa");
    let server_b = Server::start("B", "stdio-token-bbbbbbbb");
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), Permissions::from_mode(0o700)).unwrap();
    let settings = directory.path().join("mcp.json");
    descriptor(&settings, server_a.port, "stdio-token-aaaaaaaa", "A");

    let mut child = Command::new(env!("CARGO_BIN_EXE_harbor-mcp-bridge"))
        .env_clear()
        .env("HARBOR_SETTINGS", &settings)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":"init","method":"initialize",
            "params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}
        }),
    );
    let initialized = receive(&mut stdout);
    assert_eq!(
        initialized["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":"barrier","method":"ping"}),
    );
    assert_eq!(receive(&mut stdout)["id"], "barrier");

    descriptor(&settings, server_b.port, "stdio-token-bbbbbbbb", "B");
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    assert_eq!(
        receive(&mut stdout)["method"],
        "notifications/tools/list_changed"
    );
    let response = receive(&mut stdout);
    assert_eq!(response["result"]["server"], "B");
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    assert!(status.success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!stderr.contains("stdio-token-"));

    let b_records = server_b.records.lock().unwrap().clone();
    assert_eq!(
        b_records
            .iter()
            .map(|record| record.method.as_str())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
    assert_eq!(b_records[0].protocol, None);
    assert_eq!(b_records[1].protocol.as_deref(), Some(PROTOCOL));
    assert_eq!(b_records[2].protocol.as_deref(), Some(PROTOCOL));
}

#[test]
fn version_diagnostic_needs_no_descriptor() {
    let output = Command::new(env!("CARGO_BIN_EXE_harbor-mcp-bridge"))
        .env_clear()
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("harbor-mcp-bridge {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}
