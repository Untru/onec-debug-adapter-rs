mod dap;
mod debug_server;

use anyhow::{Context, Result};
use dap::{Reader, Writer, error_response, event, response};
use debug_server::DebugServer;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, stderr};

#[derive(Default)]
struct Adapter {
    next_sequence: u64,
    debug_server: Option<DebugServer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionArguments {
    debug_server_host: String,
    debug_server_port: u16,
}

impl Adapter {
    fn next_sequence(&mut self) -> u64 {
        self.next_sequence += 1;
        self.next_sequence
    }

    fn handle(&mut self, request: &Value) -> Vec<Value> {
        let command = request["command"].as_str().unwrap_or_default();
        match command {
            "initialize" => vec![response(
                request,
                self.next_sequence(),
                json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsConditionalBreakpoints": true,
                    "supportsLogPoints": true,
                    "supportsEvaluateForHovers": true,
                    "supportsTerminateRequest": true,
                }),
            )],
            "launch" | "attach" => match self.connect(request) {
                Ok(()) => vec![
                    response(request, self.next_sequence(), json!({})),
                    event(self.next_sequence(), "initialized", json!({})),
                ],
                Err(error) => vec![error_response(request, self.next_sequence(), error.to_string())],
            },
            "configurationDone" => vec![response(request, self.next_sequence(), json!({}))],
            "threads" => vec![response(request, self.next_sequence(), json!({ "threads": [] }))],
            "disconnect" | "terminate" => {
                self.debug_server = None;
                vec![response(request, self.next_sequence(), json!({}))]
            }
            _ => vec![error_response(
                request,
                self.next_sequence(),
                format!("DAP command `{command}` is not implemented yet"),
            )],
        }
    }

    fn connect(&mut self, request: &Value) -> Result<()> {
        if self.debug_server.is_some() {
            anyhow::bail!("a 1C debug server is already attached");
        }
        let arguments: ConnectionArguments = serde_json::from_value(request["arguments"].clone())
            .context("launch/attach requires debugServerHost and debugServerPort")?;
        let server = DebugServer::new(&arguments.debug_server_host, arguments.debug_server_port)?;
        server.test_connection()?;
        eprintln!("connected to 1C debug server: {}", server.endpoint());
        self.debug_server = Some(server);
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut reader = Reader::new(io::stdin().lock());
    let mut writer = Writer::new(io::stdout().lock());
    let mut adapter = Adapter::default();

    while let Some(message) = reader.read()? {
        let request = message.0;
        if request["type"] != "request" {
            eprintln!("ignoring non-request DAP message: {request}");
            continue;
        }
        for outgoing in adapter.handle(&request) {
            writer.write(&outgoing)?;
        }
    }
    let _ = stderr();
    Ok(())
}
