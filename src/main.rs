mod dap;
mod debug_server;

use anyhow::{Context, Result};
use dap::{Reader, Writer, error_response, event, response};
use debug_server::{DebugServer, DebugUiSession};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, stderr};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct Adapter {
    next_sequence: u64,
    debug_server: Option<DebugServer>,
    debug_session: Option<DebugUiSession>,
    poll_failed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionArguments {
    debug_server_host: String,
    debug_server_port: u16,
    info_base: Option<String>,
    info_base_alias: Option<String>,
    auto_attach_types: Option<Vec<String>>,
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
                Err(error) => vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )],
            },
            "configurationDone" => vec![response(request, self.next_sequence(), json!({}))],
            "threads" => vec![response(
                request,
                self.next_sequence(),
                json!({ "threads": [] }),
            )],
            "disconnect" | "terminate" => match self.disconnect() {
                Ok(()) => vec![response(request, self.next_sequence(), json!({}))],
                Err(error) => vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )],
            },
            _ => vec![error_response(
                request,
                self.next_sequence(),
                format!("DAP command `{command}` is not implemented yet"),
            )],
        }
    }

    fn connect(&mut self, request: &Value) -> Result<()> {
        if self.debug_session.is_some() {
            anyhow::bail!("a 1C debug server is already attached");
        }
        let arguments: ConnectionArguments =
            serde_json::from_value(request["arguments"].clone())
                .context("launch/attach requires debugServerHost and debugServerPort")?;
        let server = DebugServer::new(&arguments.debug_server_host, arguments.debug_server_port)?;
        let info_base_alias = arguments
            .info_base_alias
            .as_deref()
            .or(arguments.info_base.as_deref())
            .context("launch/attach requires infoBase or infoBaseAlias")?;
        let session = server.attach_debug_ui(info_base_alias)?;
        if let Some(types) = &arguments.auto_attach_types {
            server.set_auto_attach_types(&session, types)?;
        }
        eprintln!(
            "attached Debug UI {} to 1C debug server: {}",
            session.id(),
            server.endpoint()
        );
        self.debug_server = Some(server);
        self.debug_session = Some(session);
        self.poll_failed = false;
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        if let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) {
            server.detach_debug_ui(session)?;
        }
        self.debug_session = None;
        self.debug_server = None;
        self.poll_failed = false;
        Ok(())
    }

    fn poll(&mut self) -> Vec<Value> {
        let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) else {
            return Vec::new();
        };

        match server.ping_debug_ui(session) {
            Ok(events) => {
                self.poll_failed = false;
                events
                    .into_iter()
                    .map(|debug_event| {
                        event(
                            self.next_sequence(),
                            "output",
                            json!({
                                "category": "console",
                                "output": format!("1C debug event: {}\\n", debug_event.command_id),
                            }),
                        )
                    })
                    .collect()
            }
            Err(error) if !self.poll_failed => {
                self.poll_failed = true;
                vec![event(
                    self.next_sequence(),
                    "output",
                    json!({
                        "category": "stderr",
                        "output": format!("1C debug server poll failed: {error}\\n"),
                    }),
                )]
            }
            Err(_) => Vec::new(),
        }
    }
}

fn main() -> Result<()> {
    let (sender, receiver) = mpsc::channel::<std::result::Result<Value, String>>();
    thread::spawn(move || {
        let mut reader = Reader::new(io::stdin().lock());
        loop {
            match reader.read() {
                Ok(Some(message)) => {
                    if sender.send(Ok(message.0)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });

    let mut writer = Writer::new(io::stdout().lock());
    let mut adapter = Adapter::default();

    loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(request)) => {
                if request["type"] != "request" {
                    eprintln!("ignoring non-request DAP message: {request}");
                    continue;
                }
                for outgoing in adapter.handle(&request) {
                    writer.write(&outgoing)?;
                }
            }
            Ok(Err(error)) => anyhow::bail!("cannot read DAP input: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                for outgoing in adapter.poll() {
                    writer.write(&outgoing)?;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = stderr();
    Ok(())
}
