//! Minimal, dependency-light implementation of the Debug Adapter Protocol transport.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};

/// One JSON message framed according to the DAP base protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct Message(pub Value);

pub struct Reader<R> {
    input: BufReader<R>,
}

impl<R: Read> Reader<R> {
    pub fn new(input: R) -> Self {
        Self {
            input: BufReader::new(input),
        }
    }

    /// Reads a single DAP frame. `Ok(None)` means a clean end of input.
    pub fn read(&mut self) -> Result<Option<Message>> {
        let mut content_length = None;
        let mut saw_header = false;

        loop {
            let mut line = String::new();
            let bytes = self.input.read_line(&mut line)?;
            if bytes == 0 {
                if !saw_header {
                    return Ok(None);
                }
                bail!("unexpected end of input in DAP headers");
            }
            saw_header = true;

            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            let (name, value) = line
                .split_once(':')
                .context("invalid DAP header (expected `Name: value`)")?;
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("invalid Content-Length")?,
                );
            }
        }

        let content_length = content_length.context("DAP message has no Content-Length header")?;
        let mut payload = vec![0; content_length];
        self.input.read_exact(&mut payload)?;
        let value = serde_json::from_slice(&payload).context("invalid DAP JSON payload")?;
        Ok(Some(Message(value)))
    }
}

pub struct Writer<W> {
    output: W,
}

impl<W: Write> Writer<W> {
    pub fn new(output: W) -> Self {
        Self { output }
    }

    pub fn write(&mut self, message: &Value) -> Result<()> {
        let payload = serde_json::to_vec(message)?;
        write!(self.output, "Content-Length: {}\r\n\r\n", payload.len())?;
        self.output.write_all(&payload)?;
        self.output.flush()?;
        Ok(())
    }
}

pub fn response(request: &Value, sequence: u64, body: Value) -> Value {
    json!({
        "seq": sequence,
        "type": "response",
        "request_seq": request["seq"],
        "success": true,
        "command": request["command"],
        "body": body,
    })
}

pub fn error_response(request: &Value, sequence: u64, message: impl AsRef<str>) -> Value {
    json!({
        "seq": sequence,
        "type": "response",
        "request_seq": request["seq"],
        "success": false,
        "command": request["command"],
        "message": message.as_ref(),
    })
}

pub fn event(sequence: u64, name: &str, body: Value) -> Value {
    json!({ "seq": sequence, "type": "event", "event": name, "body": body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let payload = r#"{"command":"test"}"#;
        let source = format!("Content-Length: {}\r\n\r\n{payload}", payload.len());
        let received = Reader::new(source.as_bytes()).read().unwrap().unwrap();
        assert_eq!(received.0["command"], "test");

        let mut bytes = Vec::new();
        Writer::new(&mut bytes).write(&received.0).unwrap();
        assert_eq!(Reader::new(&bytes[..]).read().unwrap().unwrap(), received);
    }
}
