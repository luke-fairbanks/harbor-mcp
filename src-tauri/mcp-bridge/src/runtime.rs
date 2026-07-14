use crate::descriptor::{read_descriptor, Descriptor, DescriptorError};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect::Policy;
use serde_json::Value;
use std::ffi::{CStr, OsString};
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_HTTP_BODY_BYTES: u64 = 2 * 1024 * 1024;
const CURRENT_HEALTH_TEXT: &[u8] = b"Harbor MCP OK";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum RuntimeError {
    Descriptor,
    Unavailable,
    Unauthorized,
    /// The operation is known not to have reached Harbor and may be retried.
    PreSend,
    Ambiguous,
    Rejected,
}

#[derive(Debug)]
pub(crate) enum PostResult {
    Accepted,
    Json(Value),
}

pub(crate) trait Runtime {
    fn read_descriptor(&self) -> Result<Descriptor, RuntimeError>;
    fn validate_backend(&self, descriptor: &Descriptor) -> Result<(), RuntimeError>;
    fn post(
        &self,
        descriptor: &Descriptor,
        message: &Value,
        protocol_version: Option<&str>,
    ) -> Result<PostResult, RuntimeError>;
    fn start_harbor(&self, descriptor: Option<&Descriptor>) -> Result<(), RuntimeError>;
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

trait ListenerVerifier: Send + Sync {
    fn verify(&self, descriptor: &Descriptor) -> Result<(), RuntimeError>;
}

trait AppStarter: Send + Sync {
    fn start(&self, descriptor: Option<&Descriptor>) -> Result<(), RuntimeError>;
}

/// Production implementation of the bridge's secure descriptor and HTTP I/O.
pub struct NativeRuntime {
    settings_path: PathBuf,
    client: Client,
    listener_verifier: Arc<dyn ListenerVerifier>,
    app_starter: Arc<dyn AppStarter>,
}

/// Deliberately opaque: startup diagnostics must not echo settings paths or
/// other potentially sensitive local configuration.
#[derive(Debug)]
pub struct InitializationError;

impl std::fmt::Display for InitializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("secure bridge initialization failed")
    }
}

impl std::error::Error for InitializationError {}

impl NativeRuntime {
    /// Build a production runtime. `HARBOR_SETTINGS` remains supported for
    /// older generated client configs, while new installs find `mcp.json`
    /// beside the bridge executable.
    pub fn from_env() -> Result<Self, InitializationError> {
        let settings_path = match std::env::var_os("HARBOR_SETTINGS") {
            Some(value) => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(InitializationError);
                }
                path
            }
            None => std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("mcp.json")))
                .filter(|path| path.is_absolute())
                .ok_or(InitializationError)?,
        };

        Self::new(
            settings_path,
            Duration::from_millis(750),
            Arc::new(SystemListenerVerifier),
            Arc::new(SystemAppStarter),
        )
        .map_err(|_| InitializationError)
    }

    fn new(
        settings_path: PathBuf,
        connect_timeout: Duration,
        listener_verifier: Arc<dyn ListenerVerifier>,
        app_starter: Arc<dyn AppStarter>,
    ) -> Result<Self, RuntimeError> {
        if !settings_path.is_absolute() {
            return Err(RuntimeError::Descriptor);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|_| RuntimeError::Unavailable)?;
        Ok(Self {
            settings_path,
            client,
            listener_verifier,
            app_starter,
        })
    }

    fn endpoint(descriptor: &Descriptor, suffix: &str) -> String {
        // The descriptor controls only a validated u16 port. The host and path
        // are constants so settings contents can never redirect credentials.
        format!("http://127.0.0.1:{}{suffix}", descriptor.port)
    }

    fn health(&self, descriptor: &Descriptor) -> Result<(), RuntimeError> {
        let response = self
            .client
            .get(Self::endpoint(descriptor, "/health"))
            .bearer_auth(&descriptor.token)
            .header(ACCEPT, "application/json, text/plain")
            .timeout(Duration::from_secs(1))
            .send()
            .map_err(classify_pre_response_error)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RuntimeError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(RuntimeError::Unavailable);
        }
        let bytes = read_bounded(response).map_err(|_| RuntimeError::Unavailable)?;
        if healthy_body(&bytes) {
            Ok(())
        } else {
            Err(RuntimeError::Unavailable)
        }
    }
}

impl Runtime for NativeRuntime {
    fn read_descriptor(&self) -> Result<Descriptor, RuntimeError> {
        read_descriptor(&self.settings_path).map_err(|error| match error {
            DescriptorError::Missing
            | DescriptorError::Unsafe
            | DescriptorError::TooLarge
            | DescriptorError::Invalid
            | DescriptorError::Io => RuntimeError::Descriptor,
        })
    }

    fn validate_backend(&self, descriptor: &Descriptor) -> Result<(), RuntimeError> {
        self.listener_verifier
            .verify(descriptor)
            .map_err(|_| RuntimeError::PreSend)?;
        self.health(descriptor)
    }

    fn post(
        &self,
        descriptor: &Descriptor,
        message: &Value,
        protocol_version: Option<&str>,
    ) -> Result<PostResult, RuntimeError> {
        // Backend recovery can replay initialize/initialized before forwarding
        // the client's current request. Recheck the listener immediately before
        // every bearer-bearing POST so each of those internal requests is bound
        // to the recorded Harbor PID, UID, and process generation.
        self.listener_verifier
            .verify(descriptor)
            .map_err(|_| RuntimeError::PreSend)?;
        let mut request = self
            .client
            .post(Self::endpoint(descriptor, "/mcp"))
            .bearer_auth(&descriptor.token)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .json(message);
        if let Some(version) = protocol_version.filter(|value| valid_protocol_version(value)) {
            request = request.header("MCP-Protocol-Version", version);
        }

        let response = request.send().map_err(classify_post_error)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RuntimeError::Unauthorized);
        }
        if response.status() == reqwest::StatusCode::ACCEPTED {
            return Ok(PostResult::Accepted);
        }
        if !response.status().is_success() {
            return Err(RuntimeError::Rejected);
        }

        let body = read_bounded(response).map_err(|_| RuntimeError::Ambiguous)?;
        let value = serde_json::from_slice::<Value>(&body).map_err(|_| RuntimeError::Rejected)?;
        if !value.is_object() {
            return Err(RuntimeError::Rejected);
        }
        Ok(PostResult::Json(value))
    }

    fn start_harbor(&self, descriptor: Option<&Descriptor>) -> Result<(), RuntimeError> {
        self.app_starter.start(descriptor)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn classify_pre_response_error(error: reqwest::Error) -> RuntimeError {
    if error.is_connect() {
        RuntimeError::PreSend
    } else {
        RuntimeError::Unavailable
    }
}

fn classify_post_error(error: reqwest::Error) -> RuntimeError {
    if error.is_connect() {
        // reqwest did not establish a connection, so no handler could have
        // observed the JSON-RPC operation and one recovery retry is safe.
        RuntimeError::PreSend
    } else {
        // Timeouts and failures after connection establishment are ambiguous:
        // a tools/call may already have run. They must never be replayed.
        RuntimeError::Ambiguous
    }
}

fn read_bounded(mut response: Response) -> Result<Vec<u8>, ()> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_BODY_BYTES)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_HTTP_BODY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_HTTP_BODY_BYTES {
        Err(())
    } else {
        Ok(bytes)
    }
}

fn healthy_body(body: &[u8]) -> bool {
    if body == CURRENT_HEALTH_TEXT {
        return true;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("status").and_then(Value::as_str) == Some("ok")
        && matches!(
            object.get("service").and_then(Value::as_str),
            Some("Harbor MCP") | Some("harbor-mcp")
        )
}

fn valid_protocol_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

struct SystemListenerVerifier;

impl ListenerVerifier for SystemListenerVerifier {
    fn verify(&self, descriptor: &Descriptor) -> Result<(), RuntimeError> {
        let port = descriptor.port;
        let lsof = [Path::new("/usr/sbin/lsof"), Path::new("/usr/bin/lsof")]
            .into_iter()
            .find(|path| path.is_file())
            .ok_or(RuntimeError::Unavailable)?;
        let output = Command::new(lsof)
            .env_clear()
            .args([
                "-nP".to_string(),
                "-a".to_string(),
                format!("-iTCP:{port}"),
                "-sTCP:LISTEN".to_string(),
                "-Fpu".to_string(),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| RuntimeError::Unavailable)?;
        if !output.status.success() {
            return Err(RuntimeError::Unavailable);
        }

        let effective_uid = unsafe { libc::geteuid() };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut pids: Vec<u32> = Vec::new();
        let mut observed_uids: Vec<u32> = Vec::new();
        for line in text.lines() {
            if let Some(pid) = line.strip_prefix('p').and_then(|value| value.parse().ok()) {
                pids.push(pid);
            } else if let Some(uid) = line.strip_prefix('u').and_then(|value| value.parse().ok()) {
                observed_uids.push(uid);
            }
        }
        if pids.is_empty() {
            return Err(RuntimeError::Unavailable);
        }

        let ps = [Path::new("/bin/ps"), Path::new("/usr/bin/ps")]
            .into_iter()
            .find(|path| path.is_file())
            .ok_or(RuntimeError::Unavailable)?;
        if descriptor.pid != 0 && !pids.contains(&descriptor.pid) {
            return Err(RuntimeError::Unavailable);
        }
        if !observed_uids.is_empty() && !observed_uids.iter().all(|uid| *uid == effective_uid) {
            return Err(RuntimeError::Unavailable);
        }
        if observed_uids.is_empty() {
            for pid in &pids {
                if process_uid(ps, *pid)? != effective_uid {
                    return Err(RuntimeError::Unavailable);
                }
            }
        }
        if let (pid, Some(expected_start)) =
            (descriptor.pid, descriptor.process_started_at.as_deref())
        {
            if pid == 0 || process_started_at(ps, pid)?.trim() != expected_start.trim() {
                return Err(RuntimeError::Unavailable);
            }
        }
        Ok(())
    }
}

fn process_uid(ps: &Path, pid: u32) -> Result<u32, RuntimeError> {
    let pid_string = pid.to_string();
    let output = Command::new(ps)
        .env_clear()
        .args(["-o", "uid=", "-p", &pid_string])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| RuntimeError::Unavailable)?;
    if !output.status.success() {
        return Err(RuntimeError::Unavailable);
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| RuntimeError::Unavailable)
}

fn process_started_at(ps: &Path, pid: u32) -> Result<String, RuntimeError> {
    let pid_string = pid.to_string();
    let output = Command::new(ps)
        .env_clear()
        .args(["-o", "lstart=", "-p", &pid_string])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| RuntimeError::Unavailable)?;
    if !output.status.success() {
        return Err(RuntimeError::Unavailable);
    }
    let value = String::from_utf8(output.stdout).map_err(|_| RuntimeError::Unavailable)?;
    if value.trim().is_empty() {
        Err(RuntimeError::Unavailable)
    } else {
        Ok(value)
    }
}

struct SystemAppStarter;

impl AppStarter for SystemAppStarter {
    fn start(&self, descriptor: Option<&Descriptor>) -> Result<(), RuntimeError> {
        let preferred = descriptor
            .and_then(|descriptor| descriptor.app_executable.as_deref())
            .and_then(|executable| recorded_app_open_command(Path::new(executable)).ok());

        // Legacy descriptors have no executable, and a recorded application
        // can move during an upgrade. The fallback name and every argument are
        // fixed; settings never become shell syntax.
        spawn_preferred_or_fallback(preferred, fallback_open_command()).map(|_| ())
    }
}

fn recorded_app_open_command(executable: &Path) -> Result<Command, RuntimeError> {
    let executable = validate_harbor_executable(executable)?;
    let bundle = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or(RuntimeError::Rejected)?;
    let mut command = Command::new("/usr/bin/open");
    command
        .arg("-gj")
        .arg(bundle)
        .args(["--args", "--background-service"]);
    Ok(command)
}

fn fallback_open_command() -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.args(["-gj", "-a", "Harbor", "--args", "--background-service"]);
    command
}

fn spawn_preferred_or_fallback(
    preferred: Option<Command>,
    fallback: Command,
) -> Result<u32, RuntimeError> {
    if let Some(preferred) = preferred {
        if let Ok(pid) = spawn_with_reaper(preferred) {
            return Ok(pid);
        }
    }
    spawn_with_reaper(fallback)
}

fn spawn_with_reaper(mut command: Command) -> Result<u32, RuntimeError> {
    // Create the waiter before the short-lived `open` process. LaunchServices
    // owns the actual application process, so killing a client's MCP process
    // group cannot accidentally kill Harbor.
    let (sender, receiver) = std::sync::mpsc::sync_channel::<std::process::Child>(1);
    std::thread::Builder::new()
        .name("harbor-app-reaper".to_string())
        .spawn(move || {
            if let Ok(mut child) = receiver.recv() {
                let _ = child.wait();
            }
        })
        .map_err(|_| RuntimeError::Unavailable)?;

    configure_sanitized_environment(&mut command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RuntimeError::Unavailable)?;
    let pid = child.id();
    if let Err(error) = sender.send(child) {
        child = error.0;
        let _ = child.kill();
        let _ = child.wait();
        return Err(RuntimeError::Unavailable);
    }
    Ok(pid)
}

fn configure_sanitized_environment(command: &mut Command) {
    command.env_clear().envs([
        ("PATH", OsString::from("/usr/bin:/bin:/usr/sbin:/sbin")),
        ("TMPDIR", OsString::from("/tmp")),
    ]);
    if let Some((user, home)) = account_identity() {
        command
            .env("HOME", home)
            .env("USER", &user)
            .env("LOGNAME", user);
    }
}

fn account_identity() -> Option<(OsString, OsString)> {
    let uid = unsafe { libc::geteuid() };
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        let record = unsafe { record.assume_init() };
        if record.pw_name.is_null() || record.pw_dir.is_null() {
            return None;
        }
        let user = unsafe { CStr::from_ptr(record.pw_name) }
            .to_bytes()
            .to_vec();
        let home = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes().to_vec();
        if user.is_empty() || home.is_empty() {
            return None;
        }
        return Some((OsString::from_vec(user), OsString::from_vec(home)));
    }
}

fn validate_harbor_executable(path: &Path) -> Result<&Path, RuntimeError> {
    if !path.is_absolute()
        || !matches!(
            path.file_name().and_then(|value| value.to_str()),
            Some("Harbor") | Some("harbor")
        )
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some("MacOS")
        || path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some("Contents")
        || path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some("Harbor.app")
    {
        return Err(RuntimeError::Rejected);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| RuntimeError::Unavailable)?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || (metadata.uid() != effective_uid && metadata.uid() != 0)
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(RuntimeError::Rejected);
    }
    if let Ok(current) = std::env::current_exe().and_then(fs::metadata) {
        if current.dev() == metadata.dev() && current.ino() == metadata.ino() {
            return Err(RuntimeError::Rejected);
        }
    }
    Ok(path)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct AllowListener;

    impl ListenerVerifier for AllowListener {
        fn verify(&self, _descriptor: &Descriptor) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    pub(crate) struct CountingStarter(pub Arc<AtomicUsize>);

    impl AppStarter for CountingStarter {
        fn start(&self, _descriptor: Option<&Descriptor>) -> Result<(), RuntimeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    pub(crate) fn native_runtime(
        settings_path: PathBuf,
        starts: Arc<AtomicUsize>,
    ) -> NativeRuntime {
        NativeRuntime::new(
            settings_path,
            Duration::from_millis(100),
            Arc::new(AllowListener),
            Arc::new(CountingStarter(starts)),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn health_contract_is_narrow_and_forward_compatible() {
        assert!(healthy_body(b"Harbor MCP OK"));
        assert!(healthy_body(br#"{"status":"ok","service":"Harbor MCP"}"#));
        assert!(!healthy_body(b"ok"));
        assert!(!healthy_body(br#"{"status":"ok","service":"other"}"#));
    }

    #[test]
    fn protocol_header_rejects_control_characters() {
        assert!(valid_protocol_version("2025-11-25"));
        assert!(!valid_protocol_version("2025-11-25\r\nAuthorization: nope"));
    }

    struct RejectListener;

    impl ListenerVerifier for RejectListener {
        fn verify(&self, _descriptor: &Descriptor) -> Result<(), RuntimeError> {
            Err(RuntimeError::Rejected)
        }
    }

    #[test]
    fn every_authenticated_post_requires_fresh_listener_verification() {
        let runtime = NativeRuntime::new(
            PathBuf::from("/tmp/unused-harbor-mcp.json"),
            Duration::from_millis(10),
            Arc::new(RejectListener),
            Arc::new(test_support::CountingStarter(Arc::new(AtomicUsize::new(0)))),
        )
        .unwrap();
        let descriptor = Descriptor {
            schema_version: 1,
            instance_id: "test".to_string(),
            pid: std::process::id(),
            process_started_at: None,
            port: 9,
            token: "0123456789abcdef".to_string(),
            app_executable: None,
        };

        assert!(matches!(
            runtime.post(
                &descriptor,
                &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                Some("2025-11-25"),
            ),
            Err(RuntimeError::PreSend)
        ));
    }

    #[test]
    fn autostart_target_must_be_the_executable_harbor_bundle_binary() {
        let directory = tempfile::tempdir().unwrap();
        let macos = directory
            .path()
            .join("Harbor.app")
            .join("Contents")
            .join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let executable = macos.join("harbor");
        std::fs::write(&executable, b"test").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            validate_harbor_executable(&executable),
            Err(RuntimeError::Rejected)
        ));
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_harbor_executable(&executable).is_ok());
        let command = recorded_app_open_command(&executable).unwrap();
        assert_eq!(command.get_program(), std::ffi::OsStr::new("/usr/bin/open"));
        let expected_bundle = directory.path().join("Harbor.app");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-gj"),
                expected_bundle.as_os_str(),
                std::ffi::OsStr::new("--args"),
                std::ffi::OsStr::new("--background-service")
            ]
        );
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o722)).unwrap();
        assert!(matches!(
            validate_harbor_executable(&executable),
            Err(RuntimeError::Rejected)
        ));
    }

    #[test]
    fn sanitized_autostart_identity_has_no_inherited_secrets() {
        let mut command = Command::new("/usr/bin/true");
        command.env("ANTHROPIC_API_KEY", "must-not-survive");
        configure_sanitized_environment(&mut command);
        let variables = command.get_envs().collect::<Vec<_>>();
        assert!(variables
            .iter()
            .all(|(name, _)| *name != std::ffi::OsStr::new("ANTHROPIC_API_KEY")));
        assert!(variables
            .iter()
            .any(|(name, value)| *name == std::ffi::OsStr::new("HOME") && value.is_some()));
    }

    #[test]
    fn failed_preferred_launch_falls_back_and_child_is_reaped() {
        let preferred = Command::new("/definitely/not/a/harbor/executable");
        let fallback = Command::new("/usr/bin/true");
        let pid = spawn_preferred_or_fallback(Some(preferred), fallback).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let pid_string = pid.to_string();
            let output = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid_string])
                .output()
                .unwrap();
            if !output.status.success() || output.stdout.iter().all(u8::is_ascii_whitespace) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reaper left a child process behind"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
