mod dap;
mod debug_server;
mod metadata;

use anyhow::{Context, Result};
use dap::{Reader, Writer, error_response, event, response};
use debug_server::{
    CalculationPathItem, DebugEvaluation, DebugServer, DebugStackFrame, DebugTarget, DebugUiEvent,
    DebugUiSession, DebugVariable, EvaluationInterface, ModuleBreakpoints, SourceBreakpoint,
    StepAction,
};
use metadata::ModuleRegistry;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, stderr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

struct Adapter {
    next_sequence: u64,
    debug_server: Option<DebugServer>,
    debug_session: Option<DebugUiSession>,
    file_debug_server: Option<Child>,
    debuggee: Option<Child>,
    pending_debuggee: Option<PendingDebuggee>,
    debuggee_launcher: DebuggeeLauncher,
    poll_failed: bool,
    threads: BTreeMap<String, i64>,
    call_stacks: BTreeMap<i64, Vec<DebugStackFrame>>,
    next_variable_reference: i64,
    variable_references: BTreeMap<i64, VariableReference>,
    pending_evaluations: BTreeMap<String, PendingEvaluation>,
    module_registry: Option<ModuleRegistry>,
    module_breakpoints: BTreeMap<(String, String, String), Vec<SourceBreakpoint>>,
}

type DebuggeeLauncher = fn(&ConnectionArguments, &InfoBaseTarget, &DebugServer) -> Result<Child>;

struct PendingDebuggee {
    arguments: ConnectionArguments,
    info_base: InfoBaseTarget,
}

impl Default for Adapter {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            debug_server: None,
            debug_session: None,
            file_debug_server: None,
            debuggee: None,
            pending_debuggee: None,
            debuggee_launcher: launch_debuggee,
            poll_failed: false,
            threads: BTreeMap::new(),
            call_stacks: BTreeMap::new(),
            next_variable_reference: 0,
            variable_references: BTreeMap::new(),
            pending_evaluations: BTreeMap::new(),
            module_registry: None,
            module_breakpoints: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
enum VariableReference {
    Locals {
        thread_id: i64,
        stack_level: i64,
    },
    Value {
        thread_id: i64,
        stack_level: i64,
        path: Vec<CalculationPathItem>,
        interface: EvaluationInterface,
    },
}

#[derive(Clone)]
enum PendingEvaluation {
    Evaluate {
        request: Value,
        thread_id: i64,
        stack_level: i64,
        path: Vec<CalculationPathItem>,
    },
    Variables {
        request: Value,
        reference: VariableReference,
    },
}

impl VariableReference {
    fn address(&self) -> (i64, i64) {
        match self {
            Self::Locals {
                thread_id,
                stack_level,
            }
            | Self::Value {
                thread_id,
                stack_level,
                ..
            } => (*thread_id, *stack_level),
        }
    }

    fn path(&self) -> Vec<CalculationPathItem> {
        match self {
            Self::Locals { .. } => Vec::new(),
            Self::Value { path, .. } => path.clone(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionArguments {
    #[serde(default = "default_debug_server_host")]
    debug_server_host: String,
    #[serde(default = "default_debug_server_port")]
    debug_server_port: u16,
    info_base: Option<String>,
    info_base_alias: Option<String>,
    root_project: Option<String>,
    platform_path: Option<String>,
    platform_version: Option<String>,
    extensions: Option<Vec<String>>,
    auto_attach_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InfoBaseTarget {
    alias: String,
    is_file: bool,
    /// A direct file-base path supplied in `infoBase`. Unlike a launcher
    /// entry, this must be passed to the client with `/F` and never requires
    /// registering the base in `ibases.v8i`.
    direct_file_path: Option<PathBuf>,
}

const FILE_INFOBASE_ALIAS: &str = "DefAlias";

struct SpawnedDebugServer {
    server: DebugServer,
    child: Child,
}

fn default_debug_server_host() -> String {
    "localhost".to_owned()
}

fn default_debug_server_port() -> u16 {
    1550
}

fn launch_debuggee(
    arguments: &ConnectionArguments,
    info_base_target: &InfoBaseTarget,
    server: &DebugServer,
) -> Result<Child> {
    let platform_path = arguments
        .platform_path
        .as_deref()
        .context("launch requires platformPath")?;
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
    let debugger_url = server
        .endpoint()
        .strip_suffix("/e1crdbg")
        .unwrap_or(server.endpoint());
    let mut command = Command::new(&executable);
    command.arg("ENTERPRISE");
    if let Some(path) = &info_base_target.direct_file_path {
        command.arg("/F").arg(path);
    } else {
        command.args([
            "/IBName",
            arguments
                .info_base
                .as_deref()
                .context("launch requires infoBase")?,
        ]);
    }
    command
        .args([
            "/TCOMP",
            "-SDC",
            "/DisableStartupMessages",
            "/DisplayPerformance",
            "/TechnicalSpecialistMode",
            "/DEBUG",
            "-http",
            "-attach",
            "/DEBUGGERURL",
            debugger_url,
            "/O",
            "Normal",
        ])
        .spawn()
        .with_context(|| format!("cannot start 1C client {}", executable.display()))
}

/// Starts the 1C RDBG sidecar used by a file infobase and waits until it
/// publishes its selected port. Server infobases have a persistent RDBG
/// service, but a file infobase needs this process for each debug session.
fn launch_file_debug_server(platform_bin: &Path, host: &str) -> Result<SpawnedDebugServer> {
    let executable = platform_bin.join(if cfg!(windows) { "dbgs.exe" } else { "dbgs" });
    if !executable.is_file() {
        anyhow::bail!(
            "1C debug server executable was not found at {}",
            executable.display()
        );
    }

    let notify_path = std::env::temp_dir().join(format!(
        "onec-debug-adapter-{}.notify",
        uuid::Uuid::new_v4()
    ));
    let mut child = Command::new(&executable)
        .args([
            format!("--addr={host}"),
            "--portRange=1550:1559".to_owned(),
            format!("--ownerPID={}", std::process::id()),
            format!("--notify={}", notify_path.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot start 1C debug server {}", executable.display()))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let port = loop {
        match fs::read(&notify_path) {
            Ok(notification) => match debug_server_port_from_notification_bytes(&notification) {
                Ok(port) => break port,
                Err(error) => {
                    if Instant::now() >= deadline {
                        terminate_child(&mut child);
                        let _ = fs::remove_file(&notify_path);
                        return Err(error);
                    }
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                terminate_child(&mut child);
                let _ = fs::remove_file(&notify_path);
                return Err(error).context("cannot read 1C debug server notification");
            }
        }
        if let Some(status) = child
            .try_wait()
            .context("cannot check 1C debug server status")?
        {
            let _ = fs::remove_file(&notify_path);
            anyhow::bail!("1C debug server exited before it reported a port: {status}");
        }
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            let _ = fs::remove_file(&notify_path);
            anyhow::bail!("timed out waiting for 1C debug server to report its port");
        }
        thread::sleep(Duration::from_millis(25));
    };
    let _ = fs::remove_file(&notify_path);
    Ok(SpawnedDebugServer {
        server: DebugServer::new(host, port)?,
        child,
    })
}

fn debug_server_port_from_notification(notification: &str) -> Result<u16> {
    let (_, port) = notification
        .trim()
        .rsplit_once(':')
        .context("1C debug server notification does not contain a port")?;
    let port = port
        .parse::<u16>()
        .context("1C debug server notification contains an invalid port")?;
    if port == 0 {
        anyhow::bail!("1C debug server notification contains port 0");
    }
    Ok(port)
}

/// dbgs writes its notification as UTF-16LE with a BOM on macOS, while Linux
/// builds commonly use UTF-8. Decode both forms before extracting the
/// dynamically allocated RDBG port.
fn debug_server_port_from_notification_bytes(notification: &[u8]) -> Result<u16> {
    let text = match notification {
        [0xff, 0xfe, rest @ ..] => decode_utf16_notification(rest, true)?,
        [0xfe, 0xff, rest @ ..] => decode_utf16_notification(rest, false)?,
        _ => std::str::from_utf8(notification)
            .context("1C debug server notification is not valid UTF-8")?
            .to_owned(),
    };
    debug_server_port_from_notification(&text)
}

fn decode_utf16_notification(notification: &[u8], little_endian: bool) -> Result<String> {
    let chunks = notification.chunks_exact(2);
    if !chunks.remainder().is_empty() {
        anyhow::bail!("1C debug server notification contains incomplete UTF-16 data");
    }
    let words = chunks
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&words).context("1C debug server notification is not valid UTF-16")
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn platform_bin(platform_path: &Path, requested_version: Option<&str>) -> Result<PathBuf> {
    let executable_name = if cfg!(windows) { "1cv8c.exe" } else { "1cv8c" };
    if platform_path.join(executable_name).is_file() {
        return Ok(platform_path.to_path_buf());
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

fn info_base_target(arguments: &ConnectionArguments) -> Result<InfoBaseTarget> {
    let info_base = arguments
        .info_base
        .as_deref()
        .or(arguments.info_base_alias.as_deref())
        .context("launch/attach requires infoBase or infoBaseAlias")?;
    let direct_file_path = direct_file_infobase_path(info_base);
    let launcher_target = direct_file_path
        .is_none()
        .then(|| {
            ibases_paths().into_iter().find_map(|path| {
                fs::read_to_string(path)
                    .ok()
                    .and_then(|contents| launcher_info_base(&contents, info_base))
            })
        })
        .flatten();
    let is_file = direct_file_path.is_some()
        || launcher_target
            .as_ref()
            .is_some_and(|target| target.is_file);
    let alias = if is_file {
        FILE_INFOBASE_ALIAS.to_owned()
    } else {
        arguments
            .info_base_alias
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| launcher_target.as_ref().map(|target| target.alias.clone()))
            .unwrap_or_else(|| info_base.to_owned())
    };
    Ok(InfoBaseTarget {
        alias,
        is_file,
        direct_file_path,
    })
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

fn launcher_info_base(ibases: &str, info_base_name: &str) -> Option<InfoBaseTarget> {
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
        let Some((key, connect)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("Connect") {
            continue;
        }
        if connect
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("file=")
        {
            return Some(InfoBaseTarget {
                alias: FILE_INFOBASE_ALIAS.to_owned(),
                is_file: true,
                direct_file_path: None,
            });
        }
        return extract_connection_property(connect, "Ref").map(|alias| InfoBaseTarget {
            alias,
            is_file: false,
            direct_file_path: None,
        });
    }
    None
}

fn direct_file_infobase_path(info_base: &str) -> Option<PathBuf> {
    let path = extract_connection_property(info_base, "File")
        .map(PathBuf::from)
        .or_else(|| {
            let path = PathBuf::from(info_base.trim());
            path.is_dir().then_some(path)
        })?;
    path.is_dir().then_some(path)
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
                    "supportsSingleThreadExecutionRequests": true,
                    "supportsExceptionFilterOptions": true,
                    "exceptionBreakpointFilters": [{
                        "filter": "all",
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
            "configurationDone" => match self.launch_pending_debuggee() {
                Ok(()) => vec![response(request, self.next_sequence(), json!({}))],
                Err(error) => vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )],
            },
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
            "SetAutoAttachTargetTypesRequest" => self.set_auto_attach_target_types(request),
            "DebugTargetsRequest" => self.debug_targets(request),
            "AttachDebugTargetRequest" => self.attach_debug_target(request),
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
        let info_base = info_base_target(&arguments)?;
        let mut spawned_debug_server = if launch && info_base.is_file {
            let platform_path = arguments
                .platform_path
                .as_deref()
                .context("launch requires platformPath for a file infobase")?;
            let platform_bin = platform_bin(
                &PathBuf::from(platform_path),
                arguments.platform_version.as_deref(),
            )?;
            Some(launch_file_debug_server(
                &platform_bin,
                &arguments.debug_server_host,
            )?)
        } else {
            None
        };
        let server = spawned_debug_server
            .as_ref()
            .map(|spawned| spawned.server.clone())
            .unwrap_or(DebugServer::new(
                &arguments.debug_server_host,
                arguments.debug_server_port,
            )?);
        let session = match server.attach_debug_ui(&info_base.alias) {
            Ok(session) => session,
            Err(error) => {
                if let Some(spawned) = &mut spawned_debug_server {
                    terminate_child(&mut spawned.child);
                }
                return Err(error);
            }
        };
        let auto_attach_types = arguments.auto_attach_types.as_deref().unwrap_or_default();
        if let Err(error) = server.set_auto_attach_types(&session, auto_attach_types) {
            let _ = server.detach_debug_ui(&session);
            if let Some(spawned) = &mut spawned_debug_server {
                terminate_child(&mut spawned.child);
            }
            return Err(error);
        }
        eprintln!(
            "attached Debug UI {} to 1C debug server: {}",
            session.id(),
            server.endpoint()
        );
        self.debug_server = Some(server);
        self.debug_session = Some(session);
        self.file_debug_server = spawned_debug_server.map(|spawned| spawned.child);
        self.pending_debuggee = launch.then_some(PendingDebuggee {
            arguments,
            info_base,
        });
        self.poll_failed = false;
        self.module_registry = module_registry;
        self.module_breakpoints.clear();
        Ok(())
    }

    fn launch_pending_debuggee(&mut self) -> Result<()> {
        let Some(pending) = self.pending_debuggee.take() else {
            return Ok(());
        };
        let server = self
            .debug_server
            .as_ref()
            .context("no 1C debug server is attached")?;
        match (self.debuggee_launcher)(&pending.arguments, &pending.info_base, server) {
            Ok(debuggee) => {
                self.debuggee = Some(debuggee);
                Ok(())
            }
            Err(error) => {
                self.pending_debuggee = Some(pending);
                Err(error)
            }
        }
    }

    fn disconnect(&mut self) -> Result<()> {
        let detach_result = match (&self.debug_server, &self.debug_session) {
            (Some(server), Some(session)) => server.detach_debug_ui(session),
            _ => Ok(()),
        };
        self.debug_session = None;
        self.debug_server = None;
        self.pending_debuggee = None;
        if let Some(mut debug_server) = self.file_debug_server.take() {
            terminate_child(&mut debug_server);
        }
        if let Some(mut debuggee) = self.debuggee.take() {
            terminate_child(&mut debuggee);
        }
        self.poll_failed = false;
        self.threads.clear();
        self.call_stacks.clear();
        self.variable_references.clear();
        self.pending_evaluations.clear();
        self.module_registry = None;
        self.module_breakpoints.clear();
        detach_result
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

        let all_threads_continued = request["arguments"]["singleThread"] != Value::Bool(true);
        let body = if action == StepAction::Continue {
            json!({ "allThreadsContinued": all_threads_continued })
        } else {
            json!({})
        };
        let mut messages = vec![response(request, self.next_sequence(), body)];
        if action == StepAction::Continue {
            messages.push(event(
                self.next_sequence(),
                "continued",
                json!({ "threadId": thread_id, "allThreadsContinued": all_threads_continued }),
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

    fn set_auto_attach_target_types(&mut self, request: &Value) -> Vec<Value> {
        let Some(types) = request["arguments"]["types"].as_array() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "SetAutoAttachTargetTypesRequest requires arguments.types",
            )];
        };
        let types = match types
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("each auto-attach type must be a string")
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(types) => types,
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
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
        match server.set_auto_attach_types(session, &types) {
            Ok(()) => vec![response(request, self.next_sequence(), json!({}))],
            Err(error) => vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )],
        }
    }

    fn debug_targets(&mut self, request: &Value) -> Vec<Value> {
        let Some(server) = self.debug_server.clone() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        let Some(session) = self.debug_session.clone() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        match server.get_debug_targets(&session) {
            Ok(targets) => {
                let items = targets
                    .iter()
                    .filter(|target| !self.threads.contains_key(&target.id))
                    .map(debug_target_item)
                    .collect::<Vec<_>>();
                // Preserve the original extension's PascalCase custom-DAP
                // contract. The VS Code view accepts this shape directly.
                vec![response(
                    request,
                    self.next_sequence(),
                    json!({ "Items": items }),
                )]
            }
            Err(error) => vec![error_response(
                request,
                self.next_sequence(),
                error.to_string(),
            )],
        }
    }

    fn attach_debug_target(&mut self, request: &Value) -> Vec<Value> {
        let target_id = request["arguments"]["Id"]
            .as_str()
            .or_else(|| request["arguments"]["id"].as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(target_id) = target_id else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "AttachDebugTargetRequest requires arguments.Id",
            )];
        };
        let Some(server) = self.debug_server.clone() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };
        let Some(session) = self.debug_session.clone() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "no 1C debug session is attached",
            )];
        };

        let target = match server.get_debug_targets(&session) {
            Ok(targets) => targets.into_iter().find(|target| target.id == target_id),
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
        // The original adapter intentionally treats a target that disappeared
        // between refresh and click as a harmless no-op.
        let Some(target) = target else {
            return vec![response(request, self.next_sequence(), json!({}))];
        };
        if self.threads.contains_key(&target.id) {
            return vec![response(request, self.next_sequence(), json!({}))];
        }
        if let Err(error) = server.clear_break_on_next_statement(&session) {
            return vec![error_response(
                request,
                self.next_sequence(),
                format!("cannot clear 1C break-on-next-statement: {error}"),
            )];
        }
        if let Err(error) = server.attach_debug_targets(&session, std::slice::from_ref(&target.id))
        {
            return vec![error_response(
                request,
                self.next_sequence(),
                format!("cannot attach 1C target: {error}"),
            )];
        }
        let thread_id = self.threads.values().max().copied().unwrap_or(0) + 1;
        self.threads.insert(target.id, thread_id);
        vec![
            response(request, self.next_sequence(), json!({})),
            event(
                self.next_sequence(),
                "thread",
                json!({ "reason": "started", "threadId": thread_id }),
            ),
            event(self.next_sequence(), "DebugTargetsUpdated", json!({})),
        ]
    }

    fn target_id(&self, thread_id: i64) -> Option<&str> {
        self.threads
            .iter()
            .find_map(|(target_id, id)| (*id == thread_id).then_some(target_id.as_str()))
    }

    fn poll(&mut self) -> Vec<Value> {
        let file_debug_server_status = self
            .file_debug_server
            .as_mut()
            .and_then(|debug_server| debug_server.try_wait().ok().flatten());
        if let Some(status) = file_debug_server_status {
            self.file_debug_server = None;
            let detach_error = self.disconnect().err();
            let mut messages = vec![self.output_event(
                "stderr",
                format!("1C debug server exited unexpectedly: {status}\n"),
            )];
            if let Some(error) = detach_error {
                messages.push(self.output_event(
                    "stderr",
                    format!("cannot detach 1C Debug UI after debug server exit: {error}\n"),
                ));
            }
            messages.push(event(self.next_sequence(), "terminated", json!({})));
            return messages;
        }
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
                    format!("cannot detach 1C Debug UI after client exit: {error}\n"),
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
                        "output": format!("1C debug server poll failed: {error}\n"),
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
                if let Err(error) = server.clear_break_on_next_statement(session) {
                    return vec![self.output_event(
                        "stderr",
                        format!("cannot clear 1C break-on-next-statement: {error}\n"),
                    )];
                }
                if let Err(error) =
                    server.attach_debug_targets(session, std::slice::from_ref(&target_id))
                {
                    return vec![
                        self.output_event("stderr", format!("cannot attach 1C target: {error}\n")),
                    ];
                }
                let thread_id = self.threads.values().max().copied().unwrap_or(0) + 1;
                self.threads.insert(target_id, thread_id);
                vec![
                    event(
                        self.next_sequence(),
                        "thread",
                        json!({ "reason": "started", "threadId": thread_id }),
                    ),
                    event(self.next_sequence(), "DebugTargetsUpdated", json!({})),
                ]
            }
            ("targetQuit", Some(target_id)) => match self.threads.remove(&target_id) {
                Some(thread_id) => {
                    self.call_stacks.remove(&thread_id);
                    self.clear_thread_references(thread_id);
                    vec![
                        event(
                            self.next_sequence(),
                            "thread",
                            json!({ "reason": "exited", "threadId": thread_id }),
                        ),
                        event(self.next_sequence(), "DebugTargetsUpdated", json!({})),
                    ]
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
            ("exprEvaluated", _) => self.handle_evaluation_event(debug_event),
            (command_id, _) => {
                vec![self.output_event("console", format!("1C debug event: {command_id}\n"))]
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
                format!("received call stack for unattached 1C target {target_id}\n"),
            )];
        };
        let mut messages = Vec::new();
        if let Some(message) = debug_event.message.filter(|message| !message.is_empty()) {
            messages.push(self.output_event("console", format!("{message}\n")));
        }
        if debug_event.send_message_only {
            if let Err(error) = server.step(session, &target_id, StepAction::Continue) {
                messages.push(self.output_event(
                    "stderr",
                    format!("cannot continue 1C target after message: {error}\n"),
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
        self.clear_thread_references(thread_id);
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
        let started =
            match server.begin_evaluate_expression(session, &target_id, stack_level, expression) {
                Ok(started) => started,
                Err(error) => {
                    return vec![error_response(
                        request,
                        self.next_sequence(),
                        error.to_string(),
                    )];
                }
            };
        match started.result {
            Some(evaluation) => self.evaluation_response(
                request,
                thread_id,
                stack_level,
                vec![CalculationPathItem::Expression(expression.to_owned())],
                evaluation,
            ),
            None => {
                self.pending_evaluations.insert(
                    started.result_id,
                    PendingEvaluation::Evaluate {
                        request: request.clone(),
                        thread_id,
                        stack_level,
                        path: vec![CalculationPathItem::Expression(expression.to_owned())],
                    },
                );
                Vec::new()
            }
        }
    }

    fn scopes(&mut self, request: &Value) -> Vec<Value> {
        let Some(frame_id) = request["arguments"]["frameId"].as_i64() else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "scopes requires frameId",
            )];
        };
        let Some((thread_id, stack_level)) = self.frame_address(Some(frame_id)) else {
            return vec![error_response(
                request,
                self.next_sequence(),
                "unknown stack frame",
            )];
        };
        let reference = self.store_variable_reference(VariableReference::Locals {
            thread_id,
            stack_level,
        });
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "scopes": [{
                    "name": "Локальные",
                    "variablesReference": reference,
                    "expensive": false,
                }],
            }),
        )]
    }

    fn variables(&mut self, request: &Value) -> Vec<Value> {
        let reference = match request["arguments"]["variablesReference"]
            .as_i64()
            .and_then(|id| self.variable_references.get(&id).cloned())
        {
            Some(reference) => reference,
            None => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    "variablesReference does not identify a current scope or variable",
                )];
            }
        };
        let (thread_id, stack_level) = reference.address();
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
        let started = match &reference {
            VariableReference::Locals { .. } => {
                server.begin_evaluate_local_variables(session, &target_id, stack_level)
            }
            VariableReference::Value {
                path, interface, ..
            } => server
                .begin_evaluate_path(session, &target_id, stack_level, path, *interface)
                .map(|started| debug_server::EvaluationStart {
                    result_id: started.result_id,
                    result: started.result.map(|evaluation| evaluation.variables),
                }),
        };
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                return vec![error_response(
                    request,
                    self.next_sequence(),
                    error.to_string(),
                )];
            }
        };
        match started.result {
            Some(variables) => self.variables_response(request, &reference, variables),
            None => {
                self.pending_evaluations.insert(
                    started.result_id,
                    PendingEvaluation::Variables {
                        request: request.clone(),
                        reference,
                    },
                );
                Vec::new()
            }
        }
    }

    fn handle_evaluation_event(&mut self, debug_event: DebugUiEvent) -> Vec<Value> {
        let Some(evaluation) = debug_event.evaluation else {
            return vec![self.output_event(
                "stderr",
                "1C sent exprEvaluated without calculation data\n".to_owned(),
            )];
        };
        let Some(pending) = self.pending_evaluations.remove(&evaluation.result_id) else {
            return vec![self.output_event(
                "console",
                format!(
                    "1C sent an unsolicited expression result {}\n",
                    evaluation.result_id
                ),
            )];
        };
        match pending {
            PendingEvaluation::Evaluate {
                request,
                thread_id,
                stack_level,
                path,
            } => self.evaluation_response(&request, thread_id, stack_level, path, evaluation),
            PendingEvaluation::Variables { request, reference } => {
                self.variables_response(&request, &reference, evaluation.variables)
            }
        }
    }

    fn evaluation_response(
        &mut self,
        request: &Value,
        thread_id: i64,
        stack_level: i64,
        path: Vec<CalculationPathItem>,
        evaluation: DebugEvaluation,
    ) -> Vec<Value> {
        let variables_reference = self.value_reference(
            thread_id,
            stack_level,
            path,
            evaluation.is_expandable,
            evaluation.is_indexed_collection,
        );
        let result = evaluation.error.unwrap_or(evaluation.value);
        vec![response(
            request,
            self.next_sequence(),
            json!({
                "result": result,
                "type": evaluation.type_name,
                "variablesReference": variables_reference,
            }),
        )]
    }

    fn variables_response(
        &mut self,
        request: &Value,
        parent: &VariableReference,
        variables: Vec<DebugVariable>,
    ) -> Vec<Value> {
        let variables = variables
            .into_iter()
            .map(|variable| {
                let (thread_id, stack_level) = parent.address();
                let mut path = parent.path();
                match variable.index {
                    Some(index) => path.push(CalculationPathItem::Index(index)),
                    None => match parent {
                        VariableReference::Locals { .. } => {
                            path.push(CalculationPathItem::Expression(variable.name.clone()))
                        }
                        VariableReference::Value { .. } => {
                            path.push(CalculationPathItem::Property(variable.name.clone()))
                        }
                    },
                }
                let variables_reference = self.value_reference(
                    thread_id,
                    stack_level,
                    path,
                    variable.is_expandable,
                    variable.is_indexed_collection,
                );
                json!({
                    "name": variable.name,
                    "type": variable.type_name,
                    "value": variable.value,
                    "variablesReference": variables_reference,
                })
            })
            .collect::<Vec<_>>();
        vec![response(
            request,
            self.next_sequence(),
            json!({ "variables": variables }),
        )]
    }

    fn value_reference(
        &mut self,
        thread_id: i64,
        stack_level: i64,
        path: Vec<CalculationPathItem>,
        is_expandable: bool,
        is_indexed_collection: bool,
    ) -> i64 {
        let interface = if is_expandable {
            Some(EvaluationInterface::Context)
        } else if is_indexed_collection {
            Some(EvaluationInterface::Collection)
        } else {
            None
        };
        interface.map_or(0, |interface| {
            self.store_variable_reference(VariableReference::Value {
                thread_id,
                stack_level,
                path,
                interface,
            })
        })
    }

    fn store_variable_reference(&mut self, reference: VariableReference) -> i64 {
        self.next_variable_reference = self.next_variable_reference.max(10_000_000) + 1;
        let identifier = self.next_variable_reference;
        self.variable_references.insert(identifier, reference);
        identifier
    }

    fn clear_thread_references(&mut self, thread_id: i64) {
        self.variable_references
            .retain(|_, reference| reference.address().0 != thread_id);
        self.pending_evaluations.retain(|_, pending| match pending {
            PendingEvaluation::Evaluate {
                thread_id: pending_thread,
                ..
            } => *pending_thread != thread_id,
            PendingEvaluation::Variables { reference, .. } => reference.address().0 != thread_id,
        });
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

fn debug_target_item(target: &DebugTarget) -> Value {
    json!({
        "Id": target.id,
        "User": if target.user_name.is_empty() {
            "Неизвестный пользователь"
        } else {
            target.user_name.as_str()
        },
        "Type": debug_target_type_presentation(&target.target_type),
        "Seance": target.seance_no,
    })
}

fn debug_target_type_presentation(target_type: &str) -> &str {
    match target_type {
        "Unknown" => "Неизвестный тип",
        "Client" => "Толстый клиент",
        "ManagedClient" => "Тонкий клиент",
        "WEBClient" | "WebClient" => "Веб-клиент",
        "COMConnector" | "ComConnector" => "COM-соединение",
        "Server" => "Сервер",
        "ServerEmulation" => "Сервер (файловый вариант)",
        "WEBService" | "WebService" => "Веб-сервис",
        "HTTPService" | "HttpService" => "Http-сервис",
        "OData" => "Стандартный интерфейс OData",
        "JOB" | "Job" => "Фоновое задание",
        "JobFileMode" => "Фоновое задание (файловый вариант)",
        "MobileClient" => "Клиент (мобильное приложение)",
        "MobileServer" => "Сервер (мобильное приложение)",
        "MobileJobFileMode" => "Фоновое задание (мобильное приложение)",
        "MobileManagedClient" => "Мобильный клиент",
        "MobileManagedServer" => "Автономный сервер (мобильный клиент с автономным режимом)",
        other => other,
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
    let _ = adapter.disconnect();
    let _ = stderr();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DEBUGGEE_LAUNCH_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[cfg(unix)]
    fn test_debuggee_launcher(
        _: &ConnectionArguments,
        _: &InfoBaseTarget,
        _: &DebugServer,
    ) -> Result<Child> {
        DEBUGGEE_LAUNCH_COUNT.fetch_add(1, Ordering::SeqCst);
        Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .map_err(Into::into)
    }

    #[cfg(windows)]
    fn test_debuggee_launcher(
        _: &ConnectionArguments,
        _: &InfoBaseTarget,
        _: &DebugServer,
    ) -> Result<Child> {
        DEBUGGEE_LAUNCH_COUNT.fetch_add(1, Ordering::SeqCst);
        Command::new("cmd")
            .args(["/C", "timeout /T 60 /NOBREAK >NUL"])
            .spawn()
            .map_err(Into::into)
    }

    #[test]
    fn defers_debuggee_launch_until_configuration_done() {
        DEBUGGEE_LAUNCH_COUNT.store(0, Ordering::SeqCst);
        let mut adapter = Adapter {
            debuggee_launcher: test_debuggee_launcher,
            debug_server: Some(DebugServer::new("127.0.0.1", 1550).unwrap()),
            pending_debuggee: Some(PendingDebuggee {
                arguments: ConnectionArguments {
                    debug_server_host: "127.0.0.1".to_owned(),
                    debug_server_port: 1550,
                    info_base: Some("Demo".to_owned()),
                    info_base_alias: None,
                    root_project: None,
                    platform_path: None,
                    platform_version: None,
                    extensions: None,
                    auto_attach_types: None,
                },
                info_base: InfoBaseTarget {
                    alias: "Demo".to_owned(),
                    is_file: false,
                    direct_file_path: None,
                },
            }),
            ..Default::default()
        };

        assert_eq!(DEBUGGEE_LAUNCH_COUNT.load(Ordering::SeqCst), 0);
        let breakpoints_response = adapter.handle(&json!({
            "seq": 1,
            "type": "request",
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": "/tmp/module.bsl" },
                "breakpoints": [{ "line": 1 }],
            },
        }));
        assert!(!breakpoints_response[0]["success"].as_bool().unwrap());
        assert_eq!(DEBUGGEE_LAUNCH_COUNT.load(Ordering::SeqCst), 0);

        let configuration_done = adapter.handle(&json!({
            "seq": 2,
            "type": "request",
            "command": "configurationDone",
            "arguments": {},
        }));
        assert!(configuration_done[0]["success"].as_bool().unwrap());
        assert_eq!(DEBUGGEE_LAUNCH_COUNT.load(Ordering::SeqCst), 1);
        assert!(adapter.pending_debuggee.is_none());

        let repeated_configuration_done = adapter.handle(&json!({
            "seq": 3,
            "type": "request",
            "command": "configurationDone",
            "arguments": {},
        }));
        assert!(repeated_configuration_done[0]["success"].as_bool().unwrap());
        assert_eq!(DEBUGGEE_LAUNCH_COUNT.load(Ordering::SeqCst), 1);
        adapter.disconnect().unwrap();
    }

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
            10_000_001
        );
    }

    #[test]
    fn completes_deferred_evaluation_when_rdbg_polls_a_result() {
        let mut adapter = Adapter::default();
        let request = json!({
            "seq": 11,
            "type": "request",
            "command": "evaluate",
            "arguments": { "expression": "Counter" },
        });
        adapter.pending_evaluations.insert(
            "result-1".to_owned(),
            PendingEvaluation::Evaluate {
                request,
                thread_id: 7,
                stack_level: 0,
                path: vec![CalculationPathItem::Expression("Counter".to_owned())],
            },
        );

        let response = adapter.handle_evaluation_event(DebugUiEvent {
            command_id: "exprEvaluated".to_owned(),
            evaluation: Some(DebugEvaluation {
                result_id: "result-1".to_owned(),
                value: "42".to_owned(),
                type_name: "Number".to_owned(),
                ..DebugEvaluation::default()
            }),
            ..DebugUiEvent::default()
        });

        assert_eq!(response[0]["command"], "evaluate");
        assert!(response[0]["success"].as_bool().unwrap());
        assert_eq!(response[0]["body"]["result"], "42");
        assert!(adapter.pending_evaluations.is_empty());
    }

    #[test]
    fn assigns_expandable_values_a_calculation_path_reference() {
        let mut adapter = Adapter::default();
        let request = json!({
            "seq": 12,
            "type": "request",
            "command": "variables",
            "arguments": {},
        });
        let response = adapter.variables_response(
            &request,
            &VariableReference::Locals {
                thread_id: 7,
                stack_level: 0,
            },
            vec![DebugVariable {
                name: "Document".to_owned(),
                type_name: "DocumentObject".to_owned(),
                value: "Demo".to_owned(),
                is_expandable: true,
                ..DebugVariable::default()
            }],
        );

        let reference = response[0]["body"]["variables"][0]["variablesReference"]
            .as_i64()
            .unwrap();
        assert!(reference > 10_000_000);
        assert!(matches!(
            adapter.variable_references.get(&reference),
            Some(VariableReference::Value {
                path,
                interface: EvaluationInterface::Context,
                ..
            }) if path == &vec![CalculationPathItem::Expression("Document".to_owned())]
        ));
    }

    #[test]
    fn validates_dynamic_auto_attach_requests() {
        let mut adapter = Adapter::default();
        let invalid = json!({
            "seq": 13,
            "type": "request",
            "command": "SetAutoAttachTargetTypesRequest",
            "arguments": { "types": ["Client", 7] },
        });
        let invalid_response = adapter.handle(&invalid);
        assert!(!invalid_response[0]["success"].as_bool().unwrap());
        assert!(
            invalid_response[0]["message"]
                .as_str()
                .unwrap()
                .contains("must be a string")
        );

        let no_session = json!({
            "seq": 14,
            "type": "request",
            "command": "SetAutoAttachTargetTypesRequest",
            "arguments": { "types": [] },
        });
        let no_session_response = adapter.handle(&no_session);
        assert!(!no_session_response[0]["success"].as_bool().unwrap());
        assert_eq!(
            no_session_response[0]["message"],
            "no 1C debug session is attached"
        );
    }

    #[test]
    fn preserves_the_original_debug_targets_item_contract() {
        let item = debug_target_item(&DebugTarget {
            id: "target-1".to_owned(),
            seance_no: "42".to_owned(),
            user_name: String::new(),
            target_type: "WEBClient".to_owned(),
        });

        assert_eq!(item["Id"], "target-1");
        assert_eq!(item["Seance"], "42");
        assert_eq!(item["User"], "Неизвестный пользователь");
        assert_eq!(item["Type"], "Веб-клиент");
    }

    #[test]
    fn validates_manual_debug_target_requests_before_network_access() {
        let mut adapter = Adapter::default();
        let missing_id = json!({
            "seq": 15,
            "type": "request",
            "command": "AttachDebugTargetRequest",
            "arguments": {},
        });
        let missing_id_response = adapter.handle(&missing_id);
        assert!(!missing_id_response[0]["success"].as_bool().unwrap());
        assert!(
            missing_id_response[0]["message"]
                .as_str()
                .unwrap()
                .contains("arguments.Id")
        );

        let no_session = json!({
            "seq": 16,
            "type": "request",
            "command": "DebugTargetsRequest",
            "arguments": {},
        });
        let no_session_response = adapter.handle(&no_session);
        assert!(!no_session_response[0]["success"].as_bool().unwrap());
        assert_eq!(
            no_session_response[0]["message"],
            "no 1C debug session is attached"
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
            launcher_info_base(ibases, "FileDemo"),
            Some(InfoBaseTarget {
                alias: "DefAlias".to_owned(),
                is_file: true,
                direct_file_path: None,
            })
        );
        assert_eq!(
            launcher_info_base(ibases, "Demo"),
            Some(InfoBaseTarget {
                alias: "Accounting".to_owned(),
                is_file: false,
                direct_file_path: None,
            })
        );
        assert_eq!(launcher_info_base(ibases, "Missing"), None);
    }

    #[test]
    fn recognizes_direct_file_infobases_without_a_launcher_registration() {
        let directory =
            std::env::temp_dir().join(format!("onec-file-base-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let direct = directory.to_string_lossy().into_owned();
        let connection = format!("File=\"{direct}\";");

        assert_eq!(direct_file_infobase_path(&direct), Some(directory.clone()));
        assert_eq!(
            direct_file_infobase_path(&connection),
            Some(directory.clone())
        );

        let arguments = ConnectionArguments {
            debug_server_host: "localhost".to_owned(),
            debug_server_port: 1550,
            info_base: Some(direct),
            info_base_alias: Some("IgnoredForFileBase".to_owned()),
            root_project: None,
            platform_path: None,
            platform_version: None,
            extensions: None,
            auto_attach_types: None,
        };
        assert_eq!(
            info_base_target(&arguments).unwrap(),
            InfoBaseTarget {
                alias: FILE_INFOBASE_ALIAS.to_owned(),
                is_file: true,
                direct_file_path: Some(directory.clone()),
            }
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parses_a_port_published_by_dbgs() {
        assert_eq!(
            debug_server_port_from_notification("localhost:1556\n").unwrap(),
            1556
        );
        assert_eq!(
            debug_server_port_from_notification("[::1]:1557").unwrap(),
            1557
        );
        assert!(debug_server_port_from_notification("no-port").is_err());
        assert!(debug_server_port_from_notification("localhost:0").is_err());

        let mut utf16le = vec![0xff, 0xfe];
        utf16le.extend("127.0.0.1:1558".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            debug_server_port_from_notification_bytes(&utf16le).unwrap(),
            1558
        );
    }

    #[cfg(unix)]
    #[test]
    fn starts_file_infobase_debug_server_and_uses_its_published_port() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("onec-dbgs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("dbgs");
        fs::write(
            &executable,
            r#"#!/bin/sh
for argument in "$@"; do
    case "$argument" in
        --notify=*) notify="${argument#--notify=}" ;;
    esac
done
printf '127.0.0.1:1556' > "$notify"
while true; do sleep 1; done
"#,
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

        let mut spawned = launch_file_debug_server(&root, "127.0.0.1").unwrap();
        assert_eq!(spawned.server.endpoint(), "http://127.0.0.1:1556/e1crdbg");
        assert!(spawned.child.try_wait().unwrap().is_none());
        terminate_child(&mut spawned.child);
        fs::remove_dir_all(root).unwrap();
    }
}
