use harbor_mcp_bridge::{Bridge, NativeRuntime};
use std::io::{self, BufRead, BufWriter, Write};

const MAX_STDIO_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const PARSE_ERROR: &str =
    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#;
const INVALID_REQUEST: &str =
    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}"#;

enum Frame {
    Message(Vec<u8>),
    Oversized,
}

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!("harbor-mcp-bridge {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let runtime = match NativeRuntime::from_env() {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("Harbor MCP bridge could not initialize securely.");
            std::process::exit(1);
        }
    };
    let mut bridge = Bridge::new(runtime);
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = BufWriter::new(io::stdout().lock());

    loop {
        let frame = match next_frame(&mut stdin) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) => {
                eprintln!("Harbor MCP bridge lost its input stream.");
                break;
            }
        };
        let outputs = match frame {
            Frame::Oversized => vec![INVALID_REQUEST.to_string()],
            Frame::Message(bytes) => match std::str::from_utf8(&bytes) {
                Ok(line) => bridge.process_line(line),
                Err(_) => vec![PARSE_ERROR.to_string()],
            },
        };
        if write_outputs(&mut stdout, outputs).is_err() {
            return;
        }
    }
}

fn write_outputs(writer: &mut impl Write, outputs: Vec<String>) -> io::Result<()> {
    for output in outputs {
        writeln!(writer, "{output}")?;
        writer.flush()?;
    }
    Ok(())
}

/// Read and discard-to-newline in bounded chunks. An untrusted client can
/// therefore send an oversized record without forcing unbounded allocation or
/// desynchronizing every valid record that follows it.
fn next_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut message = Vec::new();
    let mut oversized = false;
    let mut observed_input = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !observed_input {
                return Ok(None);
            }
            break;
        }
        observed_input = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if !oversized {
            let remaining = MAX_STDIO_MESSAGE_BYTES
                .saturating_add(1)
                .saturating_sub(message.len());
            message.extend_from_slice(&available[..content_len.min(remaining)]);
            if content_len > remaining || message.len() > MAX_STDIO_MESSAGE_BYTES {
                oversized = true;
                message.clear();
            }
        }
        let consumed = content_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if oversized {
        Ok(Some(Frame::Oversized))
    } else {
        if message.last() == Some(&b'\r') {
            message.pop();
        }
        Ok(Some(Frame::Message(message)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn oversized_frame_is_discarded_without_losing_next_message() {
        let mut bytes = vec![b'x'; MAX_STDIO_MESSAGE_BYTES + 1];
        bytes.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let mut input = Cursor::new(bytes);
        assert!(matches!(
            next_frame(&mut input).unwrap(),
            Some(Frame::Oversized)
        ));
        let Some(Frame::Message(next)) = next_frame(&mut input).unwrap() else {
            panic!("expected the valid second frame");
        };
        assert_eq!(
            std::str::from_utf8(&next).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#
        );
        assert!(next_frame(&mut input).unwrap().is_none());
    }
}
