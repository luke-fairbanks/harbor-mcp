//! Health probes (DESIGN.md §3.1). One-shot checks the supervisor polls; the
//! `Log` variant is handled in the log reader, not here.

use crate::model::HealthCheck;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Run a single health check. Returns true if the service looks ready.
pub async fn check_once(hc: &HealthCheck, port: Option<u16>) -> bool {
    match hc {
        HealthCheck::Tcp => match port {
            Some(p) => tcp_ok(p).await,
            None => false,
        },
        HealthCheck::Http { path, .. } => match port {
            Some(p) => http_ok(p, path).await,
            None => false,
        },
        // "alive" — handled by the monitor (process running == ready).
        HealthCheck::Process => true,
        // readiness comes from a log line; not a network probe.
        HealthCheck::Log { .. } => false,
    }
}

async fn tcp_ok(port: u16) -> bool {
    timeout(
        Duration::from_millis(800),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Minimal dependency-free HTTP/1.0 GET; ready on a 2xx/3xx status line.
async fn http_ok(port: u16, path: &str) -> bool {
    let path = if path.is_empty() { "/" } else { path };
    let fut = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let req = format!(
            "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = [0u8; 128];
        let n = s.read(&mut buf).await.ok()?;
        if n == 0 {
            return Some(false);
        }
        let head = String::from_utf8_lossy(&buf[..n]);
        // "HTTP/1.1 200 OK" → take the numeric token.
        let code: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
        Some((200..400).contains(&code))
    };
    timeout(Duration::from_millis(1500), fut)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}
