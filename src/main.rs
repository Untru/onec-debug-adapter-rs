mod dap;
mod debug_server;
mod metadata;

use anyhow::{Context, Result};
use dap::{Reader, Writer, error_response, event, response};
use debug_server::{DebugServer, DebugUiSession, ModuleBreakpoints, SourceBreakpoint, StepAction};
use metadata::ModuleRegistry;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{self, stderr};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct Adapter {
    next_sequence: u64,
    debug_server: Option<DebugServer>,
    debug_session: Option<DebugUiSession>,
    poll_failed: bool,
    threads: BTreeMap<String, i64>,
    module_registry: Option<ModuleRegistry>,
    module_breakpoints: BTreeMap<(String, String, String), Vec<SourceBreakpoint>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionArguments {
    debug_server_host: String,
    debug_server_port: u16,
    info_base: Option<String>,
    info_base_alias: Option<String>,
    root_project: Option<String>,
    extensions: Option<Vec<String>>,
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
                    "supportsHitConditionalBreakpoints": true,
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
                json!({
                    "threads": self.threads.iter().map(|(target_id, thread_id)| json!({
                        "id": thread_id,
                        "name": format!("1C target {target_id}"),
                    })).collect::<Vec<_>>(),
                }),
            )],
            "continue" => self.step(request, StepAction::Continue),
            "next" => self.step(request, StepAction::Next),
            "stepIn" => self.step(request, StepAction::StepIn),
            "stepOut" => self.step(request, StepAction::StepOut),
            "pause" => self.pause(request),
            "setBreakpoints" => self.set_breakpoints(request),
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
        let module_registry = match arguments.root_project.as_deref() {
            Some(root_project) => {
                let extensions = arguments
                    .extensions
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                Some(ModuleRegistry::load(
                    &PathBuf::from(root_project),
                    &extensions,
                )?)
            }
            None => None,
        };
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
        self.module_registry = module_registry;
        self.module_breakpoints.clear();
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        if let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) {
            server.detach_debug_ui(session)?;
        }
        self.debug_session = None;
        self.debug_server = None;
        self.poll_failed = false;
        self.threads.clear();
        self.module_registry = None;
        self.module_breakpoints.clear();
        Ok(())
    }

    fn step(&mut self, request: &Value, action: StepAction) -> Vec<Value> {
        let thread_id = match request["arguments"]["threadId"].as_i64() {
            Some(thread_id) => thread_id,
            None => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "step request requires threadId",
                )];
            }
        };
        let target_id = match self.target_id(thread_id) {
            Some(target_id) => target_id.to_owned(),
            None => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    format!("unknown 1C debug thread {thread_id}"),
                )];
            }
        };
        let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };

        if let Err(error) = server.step(session, &target_id, action) {
            return vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )];
        }

        let body = if action == StepAction::Continue {
            json!({ "allThreadsContinued": false })
        } else {
            json!({})
        };
        let mut messages = vec![response(request, self.next_sequence(), body)];
        if action == StepAction::Continue {
            messages.push(event(
                self.next_sequence(),
                "continued",
                json!({ "threadId": thread_id, "allThreadsContinued": false }),
            ));
        }
        messages
    }

    fn pause(&mut self, request: &Value) -> Vec<Value> {
        let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        match server.break_on_next_statement(session) {
            Ok(()) => vec![response(request, self.next_sequence(), json!({}))],
            Err(error) => vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )],
        }
    }

    fn set_breakpoints(&mut self, request: &Value) -> Vec<Value> {
        let source_path = match request["arguments"]["source"]["path"].as_str() {
            Some(path) if !path.is_empty() => path,
            _ => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "setBreakpoints requires arguments.source.path",
                )];
            }
        };
        let registry = match &self.module_registry {
            Some(registry) => registry,
            None => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "setBreakpoints requires rootProject in the launch configuration",
                )];
            }
        };
        let module = match registry.module_by_path(&PathBuf::from(source_path)) {
            Ok(module) => module.clone(),
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
        let breakpoints = match dap_source_breakpoints(request) {
            Ok(breakpoints) => breakpoints,
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
        let key = (
            module.extension_name.clone(),
            module.object_id.clone(),
            module.property_id.clone(),
        );
        self.module_breakpoints.insert(key, breakpoints.clone());
        let modules = self
            .module_breakpoints
            .iter()
            .map(
                |((extension_name, object_id, property_id), breakpoints)| ModuleBreakpoints {
                    extension_name: extension_name.clone(),
                    object_id: object_id.clone(),
                    property_id: property_id.clone(),
                    breakpoints: breakpoints.clone(),
                },
            )
            .collect::<Vec<_>>();
        let Some(server) = &self.debug_server else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        let Some(session) = &self.debug_session else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        if let Err(error) = server.set_breakpoints(session, &modules) {
            return vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )];
        }
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "breakpoints": breakpoints.into_iter().map(|breakpoint| json!({
                    "verified": true,
                    "line": breakpoint.line,
                    "source": { "path": source_path },
                })).collect::<Vec<_>>(),
            }),
        )]
    }

    fn target_id(&self, thread_id: i64) -> Option<&str> {
        self.threads
            .iter()
            .find_map(|(target_id, id)| (*id == thread_id).then_some(target_id.as_str()))
    }

    fn poll(&mut self) -> Vec<Value> {
        let (Some(server), Some(session)) = (&self.debug_server, &self.debug_session) else {
            return Vec::new();
        };
        let server = server.clone();
        let session = session.clone();

        match server.ping_debug_ui(&session) {
            Ok(events) => {
                self.poll_failed = false;
                events
                    .into_iter()
                    .flat_map(|debug_event| {
                        self.handle_debug_event(
                            &server,
                            &session,
                            debug_event.command_id,
                            debug_event.target_id,
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

    fn handle_debug_event(
        &mut self,
        server: &DebugServer,
        session: &DebugUiSession,
        command_id: String,
        target_id: Option<String>,
    ) -> Vec<Value> {
        match (command_id.as_str(), target_id) {
            ("targetStarted", Some(target_id)) => {
                if self.threads.contains_key(&target_id) {
                    return Vec::new();
                }
                if let Err(error) =
                    server.attach_debug_targets(session, std::slice::from_ref(&target_id))
                {
                    return vec![
                        self.output_event("stderr", format!("cannot attach 1C target: {error}\\n")),
                    ];
                }
                let thread_id = self.threads.values().max().copied().unwrap_or(0) + 1;
                self.threads.insert(target_id, thread_id);
                vec![event(
                    self.next_sequence(),
                    "thread",
                    json!({ "reason": "started", "threadId": thread_id }),
                )]
            }
            ("targetQuit", Some(target_id)) => match self.threads.remove(&target_id) {
                Some(thread_id) => vec![event(
                    self.next_sequence(),
                    "thread",
                    json!({ "reason": "exited", "threadId": thread_id }),
                )],
                None => Vec::new(),
            },
            (command_id, _) => {
                vec![self.output_event("console", format!("1C debug event: {command_id}\\n"))]
            }
        }
    }

    fn output_event(&mut self, category: &str, output: String) -> Value {
        event(
            self.next_sequence(),
            "output",
            json!({ "category": category, "output": output }),
        )
    }
}

fn dap_source_breakpoints(request: &Value) -> Result<Vec<SourceBreakpoint>> {
    let breakpoints = request["arguments"]["breakpoints"]
        .as_array()
        .context("setBreakpoints requires arguments.breakpoints")?;
    breakpoints
        .iter()
        .map(|breakpoint| {
            let line = breakpoint["line"]
                .as_i64()
                .filter(|line| *line > 0)
                .context("each source breakpoint requires a positive line number")?;
            let condition = breakpoint["condition"].as_str().map(str::to_owned);
            let hit_condition = breakpoint["hitCondition"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    value
                        .parse::<i64>()
                        .context("hitCondition must be an integer")
                })
                .transpose()?;
            let log_message = breakpoint["logMessage"].as_str().map(str::to_owned);
            Ok(SourceBreakpoint {
                line,
                condition,
                hit_condition,
                log_message,
            })
        })
        .collect()
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
