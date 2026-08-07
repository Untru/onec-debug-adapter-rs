mod dap;
mod debug_server;
mod metadata;

use anyhow::{Context, Result};
use dap::{Reader, Writer, error_response, event, response};
use debug_server::{
    DebugServer, DebugStackFrame, DebugUiEvent, DebugUiSession, ModuleBreakpoints,
    SourceBreakpoint, StepAction,
};
use metadata::ModuleRegistry;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, stderr};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct Adapter {
    next_sequence: u64,
    debug_server: Option<DebugServer>,
    debug_session: Option<DebugUiSession>,
    debuggee: Option<Child>,
    poll_failed: bool,
    threads: BTreeMap<String, i64>,
    call_stacks: BTreeMap<i64, Vec<DebugStackFrame>>,
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
    platform_path: Option<String>,
    platform_version: Option<String>,
    extensions: Option<Vec<String>>,
    auto_attach_types: Option<Vec<String>>,
}

fn launch_debuggee(arguments: &ConnectionArguments, server: &DebugServer) -> Result<Child> {
    let platform_path = arguments
        .platform_path
        .as_deref()
        .context("launch requires platformPath")?;
    let info_base = arguments
        .info_base
        .as_deref()
        .context("launch requires infoBase")?;
    let platform_bin = platform_bin(
        &PathBuf::from(platform_path),
        arguments.platform_version.as_deref(),
    )?;
    let executable = platform_bin.join(if cfg!(windows) { "1cv8c.exe" } else { "1cv8c" });
    if !executable.is_file() {
        anyhow::bail!(
            "1C client executable was not found at {}",
            executable.display()
        );
    }
    Command::new(&executable)
        .args([
            "/IBName",
            info_base,
            "/TCOMP",
            "-SDC",
            "/DisableStartupMessages",
            "/DisplayPerformance",
            "/TechnicalSpecialistMode",
            "/DEBUG",
            "-http",
            "-attach",
            "/DEBUGGERURL",
            &format!("http://{}", server.endpoint().trim_end_matches("/e1crdbg")),
            "/O",
            "Normal",
        ])
        .spawn()
        .with_context(|| format!("cannot start 1C client {}", executable.display()))
}

fn platform_bin(platform_path: &PathBuf, requested_version: Option<&str>) -> Result<PathBuf> {
    let executable_name = if cfg!(windows) { "1cv8c.exe" } else { "1cv8c" };
    if platform_path.join(executable_name).is_file() {
        return Ok(platform_path.clone());
    }
    let mut versions = fs::read_dir(platform_path)
        .with_context(|| format!("cannot read platformPath {}", platform_path.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            entry
                .file_name()
                .into_string()
                .ok()
                .map(|name| (name, entry.path()))
        })
        .filter(|(name, _)| name.split('.').all(|part| part.parse::<u32>().is_ok()))
        .collect::<Vec<_>>();
    versions.sort_by_key(|entry| std::cmp::Reverse(version_key(&entry.0)));
    let selected = match requested_version.filter(|version| !version.eq_ignore_ascii_case("latest"))
    {
        Some(version) => versions
            .into_iter()
            .find(|(name, _)| name == version)
            .map(|(_, path)| path)
            .with_context(|| format!("1C platform version {version} was not found"))?,
        None => versions
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .context("platformPath contains no 1C version directories")?,
    };
    Ok(if cfg!(windows) {
        selected.join("bin")
    } else {
        selected
    })
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or_default())
        .collect()
}

fn info_base_alias(arguments: &ConnectionArguments) -> Result<String> {
    if let Some(alias) = arguments
        .info_base_alias
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return Ok(alias.to_owned());
    }
    let info_base = arguments
        .info_base
        .as_deref()
        .context("launch/attach requires infoBase or infoBaseAlias")?;
    for path in ibases_paths() {
        if let Ok(contents) = fs::read_to_string(path) {
            if let Some(alias) = launcher_alias(&contents, info_base) {
                return Ok(alias);
            }
        }
    }
    Ok(info_base.to_owned())
}

fn ibases_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if cfg!(windows) {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            paths.push(
                PathBuf::from(app_data)
                    .join("1C")
                    .join("1CEStart")
                    .join("ibases.v8i"),
            );
        }
    } else if let Some(home_dir) = std::env::var_os("HOME") {
        let home_dir = PathBuf::from(home_dir);
        paths.push(
            home_dir
                .join(".1cv8")
                .join("1C")
                .join("1CEStart")
                .join("ibases.v8i"),
        );
        paths.push(
            home_dir
                .join("Library")
                .join("Application Support")
                .join("1C")
                .join("1CEStart")
                .join("ibases.v8i"),
        );
    }
    paths
}

fn launcher_alias(ibases: &str, info_base_name: &str) -> Option<String> {
    let mut selected = false;
    for raw_line in ibases.lines() {
        let line = raw_line.trim();
        if let Some(section_name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            selected = section_name.eq_ignore_ascii_case(info_base_name);
            continue;
        }
        if !selected {
            continue;
        }
        let Some(connect) = line.strip_prefix("Connect=") else {
            continue;
        };
        if connect
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("file=")
        {
            return Some("DefAlias".to_owned());
        }
        return extract_connection_property(connect, "Ref");
    }
    None
}

fn extract_connection_property(connection: &str, property: &str) -> Option<String> {
    connection.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key.trim().eq_ignore_ascii_case(property) {
            Some(value.trim().trim_matches('"').to_owned())
        } else {
            None
        }
    })
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
                    "exceptionBreakpointFilters": [{
                        "filter": "runtimeErrors",
                        "label": "1C runtime errors",
                        "default": false,
                    }],
                }),
            )],
            "launch" | "attach" => match self.connect(request, command == "launch") {
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
            "stackTrace" => self.stack_trace(request),
            "scopes" => self.scopes(request),
            "variables" => self.variables(request),
            "continue" => self.step(request, StepAction::Continue),
            "next" => self.step(request, StepAction::Next),
            "stepIn" => self.step(request, StepAction::StepIn),
            "stepOut" => self.step(request, StepAction::StepOut),
            "pause" => self.pause(request),
            "setBreakpoints" => self.set_breakpoints(request),
            "setExceptionBreakpoints" => self.set_exception_breakpoints(request),
            "evaluate" => self.evaluate(request),
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

    fn connect(&mut self, request: &Value, launch: bool) -> Result<()> {
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
        let info_base_alias = info_base_alias(&arguments)?;
        let session = server.attach_debug_ui(&info_base_alias)?;
        if let Some(types) = &arguments.auto_attach_types {
            server.set_auto_attach_types(&session, types)?;
        }
        let debuggee = if launch {
            match launch_debuggee(&arguments, &server) {
                Ok(debuggee) => Some(debuggee),
                Err(error) => {
                    let _ = server.detach_debug_ui(&session);
                    return Err(error);
                }
            }
        } else {
            None
        };
        eprintln!(
            "attached Debug UI {} to 1C debug server: {}",
            session.id(),
            server.endpoint()
        );
        self.debug_server = Some(server);
        self.debug_session = Some(session);
        self.debuggee = debuggee;
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
        if let Some(mut debuggee) = self.debuggee.take() {
            let _ = debuggee.kill();
            let _ = debuggee.wait();
        }
        self.poll_failed = false;
        self.threads.clear();
        self.call_stacks.clear();
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

    fn set_exception_breakpoints(&mut self, request: &Value) -> Vec<Value> {
        let filters = request["arguments"]["filters"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let filter_options = request["arguments"]["filterOptions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let enabled = !filters.is_empty() || !filter_options.is_empty();
        let error_template = filter_options
            .iter()
            .find_map(|option| option["condition"].as_str())
            .filter(|condition| !condition.is_empty());
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
        if let Err(error) = server.set_runtime_error_processing(session, enabled, error_template) {
            return vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )];
        }
        vec![response(
            request,
            self.next_sequence(),
            json!({ "breakpoints": [{ "verified": true }] }),
        )]
    }

    fn target_id(&self, thread_id: i64) -> Option<&str> {
        self.threads
            .iter()
            .find_map(|(target_id, id)| (*id == thread_id).then_some(target_id.as_str()))
    }

    fn poll(&mut self) -> Vec<Value> {
        let debuggee_status = self
            .debuggee
            .as_mut()
            .and_then(|debuggee| debuggee.try_wait().unwrap_or(None));
        if let Some(status) = debuggee_status {
            self.debuggee = None;
            let detach_error = self.disconnect().err();
            let mut messages = vec![event(
                self.next_sequence(),
                "exited",
                json!({ "exitCode": status.code().unwrap_or(-1) }),
            )];
            if let Some(error) = detach_error {
                messages.push(self.output_event(
                    "stderr",
                    format!("cannot detach 1C Debug UI after client exit: {error}\\n"),
                ));
            }
            messages.push(event(self.next_sequence(), "terminated", json!({})));
            return messages;
        }
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
                    .flat_map(|debug_event| self.handle_debug_event(&server, &session, debug_event))
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
        debug_event: DebugUiEvent,
    ) -> Vec<Value> {
        match (
            debug_event.command_id.as_str(),
            debug_event.target_id.clone(),
        ) {
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
                Some(thread_id) => {
                    self.call_stacks.remove(&thread_id);
                    vec![event(
                        self.next_sequence(),
                        "thread",
                        json!({ "reason": "exited", "threadId": thread_id }),
                    )]
                }
                None => Vec::new(),
            },
            ("callStackFormed", Some(target_id)) => {
                self.handle_call_stack_event(server, session, target_id, debug_event, None)
            }
            ("rteProcessing" | "rteOnBPConditionProcessing", Some(target_id)) => self
                .handle_call_stack_event(
                    server,
                    session,
                    target_id,
                    debug_event,
                    Some("exception"),
                ),
            (command_id, _) => {
                vec![self.output_event("console", format!("1C debug event: {command_id}\\n"))]
            }
        }
    }

    fn handle_call_stack_event(
        &mut self,
        server: &DebugServer,
        session: &DebugUiSession,
        target_id: String,
        debug_event: DebugUiEvent,
        stopped_reason: Option<&str>,
    ) -> Vec<Value> {
        let Some(thread_id) = self.threads.get(&target_id).copied() else {
            return vec![self.output_event(
                "stderr",
                format!("received call stack for unattached 1C target {target_id}\\n"),
            )];
        };
        let mut messages = Vec::new();
        if let Some(message) = debug_event.message.filter(|message| !message.is_empty()) {
            messages.push(self.output_event("console", format!("{message}\\n")));
        }
        if debug_event.send_message_only {
            if let Err(error) = server.step(session, &target_id, StepAction::Continue) {
                messages.push(self.output_event(
                    "stderr",
                    format!("cannot continue 1C target after message: {error}\\n"),
                ));
            }
            return messages;
        }
        if debug_event.send_hit_counter_only {
            return messages;
        }

        let mut stack = debug_event.call_stack;
        stack.reverse();
        self.call_stacks.insert(thread_id, stack);
        let reason = stopped_reason.unwrap_or(
            if debug_event.stopped_by_breakpoint || debug_event.suspended_by_other {
                "breakpoint"
            } else {
                "step"
            },
        );
        messages.push(event(
            self.next_sequence(),
            "stopped",
            json!({ "reason": reason, "threadId": thread_id, "allThreadsStopped": false }),
        ));
        messages
    }

    fn stack_trace(&mut self, request: &Value) -> Vec<Value> {
        let thread_id = match request["arguments"]["threadId"].as_i64() {
            Some(thread_id) => thread_id,
            None => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "stackTrace requires threadId",
                )];
            }
        };
        let Some(stack) = self.call_stacks.get(&thread_id) else {
            return vec![response(
                request,
                self.next_sequence(),
                json!({ "stackFrames": [], "totalFrames": 0 }),
            )];
        };
        let start_frame = request["arguments"]["startFrame"].as_u64().unwrap_or(0) as usize;
        let levels = request["arguments"]["levels"]
            .as_u64()
            .map(|levels| levels as usize)
            .unwrap_or(usize::MAX);
        let total_frames = stack.len();
        let frames = stack
            .iter()
            .enumerate()
            .skip(start_frame)
            .take(levels)
            .map(|(index, frame)| {
                let name = if frame.presentation.is_empty() {
                    format!("1C module {}", frame.object_id)
                } else {
                    frame.presentation.clone()
                };
                let mut value = json!({
                    "id": thread_id.saturating_mul(1_000_000).saturating_add(index as i64 + 1),
                    "name": name,
                    "line": frame.line.max(1),
                    "column": 1,
                });
                if let Some(path) = self.module_registry.as_ref().and_then(|registry| {
                    registry.path_by_module(
                        &frame.extension_name,
                        &frame.object_id,
                        &frame.property_id,
                    )
                }) {
                    value["source"] = json!({ "path": path.to_string_lossy() });
                }
                value
            })
            .collect::<Vec<_>>();
        vec![response(
            request,
            self.next_sequence(),
            json!({ "stackFrames": frames, "totalFrames": total_frames }),
        )]
    }

    fn evaluate(&mut self, request: &Value) -> Vec<Value> {
        let expression = match request["arguments"]["expression"].as_str() {
            Some(expression) if !expression.trim().is_empty() => expression,
            _ => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "evaluate requires a non-empty expression",
                )];
            }
        };
        let (thread_id, stack_level) =
            match self.frame_address(request["arguments"]["frameId"].as_i64()) {
                Some(address) => address,
                None => {
                    return vec![error_response(
                        request,
                        self.next_sequence(),
                        "evaluate requires a stack frame returned by stackTrace",
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
        let evaluation =
            match server.evaluate_expression(session, &target_id, stack_level, expression) {
                Ok(evaluation) => evaluation,
                Err(error) => {
                    return vec![error_response(
                        request,
                        self.next_sequence(),
                        error.to_string(),
                    )];
                }
            };
        let result = evaluation.error.unwrap_or(evaluation.value);
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "result": result,
                "type": evaluation.type_name,
                "variablesReference": 0,
            }),
        )]
    }

    fn scopes(&mut self, request: &Value) -> Vec<Value> {
        let Some(frame_id) = request["arguments"]["frameId"].as_i64() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "scopes requires frameId",
            )];
        };
        if self.frame_address(Some(frame_id)).is_none() {
            return vec![error_response(
                request,
                self.next_sequence(),
                "unknown stack frame",
            )];
        }
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "scopes": [{
                    "name": "Локальные",
                    "variablesReference": frame_id,
                    "expensive": false,
                }],
            }),
        )]
    }

    fn variables(&mut self, request: &Value) -> Vec<Value> {
        let (thread_id, stack_level) =
            match self.frame_address(request["arguments"]["variablesReference"].as_i64()) {
                Some(address) => address,
                None => {
                    return vec![error_response(
                        request,
                        self.next_sequence(),
                        "variablesReference does not identify a current stack frame",
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
        let variables = match server.evaluate_local_variables(session, &target_id, stack_level) {
            Ok(variables) => variables,
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "variables": variables.into_iter().map(|variable| json!({
                    "name": variable.name,
                    "type": variable.type_name,
                    "value": variable.value,
                    "variablesReference": 0,
                })).collect::<Vec<_>>(),
            }),
        )]
    }

    fn frame_address(&self, frame_id: Option<i64>) -> Option<(i64, i64)> {
        let frame_id = frame_id?;
        if frame_id <= 1_000_000 {
            return None;
        }
        let thread_id = frame_id / 1_000_000;
        let stack_level = frame_id % 1_000_000 - 1;
        self.call_stacks.get(&thread_id).and_then(|stack| {
            ((stack_level as usize) < stack.len()).then_some((thread_id, stack_level))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_rdbg_call_stack_as_dap_stack_frames() {
        let mut adapter = Adapter::default();
        adapter.call_stacks.insert(
            7,
            vec![DebugStackFrame {
                extension_name: String::new(),
                object_id: "module-id".to_owned(),
                property_id: "property-id".to_owned(),
                line: 42,
                presentation: "DoWork".to_owned(),
            }],
        );
        let request = json!({
            "seq": 9,
            "type": "request",
            "command": "stackTrace",
            "arguments": { "threadId": 7 },
        });

        let response = adapter.stack_trace(&request);

        assert_eq!(response[0]["body"]["totalFrames"], 1);
        assert_eq!(response[0]["body"]["stackFrames"][0]["name"], "DoWork");
        assert_eq!(response[0]["body"]["stackFrames"][0]["line"], 42);
    }

    #[test]
    fn exposes_locals_scope_for_a_returned_stack_frame() {
        let mut adapter = Adapter::default();
        adapter
            .call_stacks
            .insert(7, vec![DebugStackFrame::default()]);
        let request = json!({
            "seq": 10,
            "type": "request",
            "command": "scopes",
            "arguments": { "frameId": 7_000_001 },
        });

        let response = adapter.scopes(&request);

        assert_eq!(response[0]["body"]["scopes"][0]["name"], "Локальные");
        assert_eq!(
            response[0]["body"]["scopes"][0]["variablesReference"],
            7_000_001
        );
    }

    #[test]
    fn finds_latest_or_requested_1c_platform_version() {
        let root = std::env::temp_dir().join(format!("onec-platform-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("8.3.9.1")).unwrap();
        fs::create_dir_all(root.join("8.3.10.1")).unwrap();

        assert_eq!(
            platform_bin(&root, None).unwrap(),
            root.join(if cfg!(windows) {
                "8.3.10.1/bin"
            } else {
                "8.3.10.1"
            })
        );
        assert_eq!(
            platform_bin(&root, Some("8.3.9.1")).unwrap(),
            root.join(if cfg!(windows) {
                "8.3.9.1/bin"
            } else {
                "8.3.9.1"
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn derives_server_alias_from_the_1c_launcher_connection() {
        let ibases = r#"
[Demo]
Connect=Srvr="localhost";Ref="Accounting";

[FileDemo]
Connect=File="/tmp/demo";
"#;

        assert_eq!(
            launcher_alias(ibases, "Demo"),
            Some("Accounting".to_owned())
        );
        assert_eq!(
            launcher_alias(ibases, "FileDemo"),
            Some("DefAlias".to_owned())
        );
        assert_eq!(launcher_alias(ibases, "Missing"), None);
    }
}
