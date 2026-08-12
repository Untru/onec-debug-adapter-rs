//! HTTP boundary for the 1C debug server.
//!
//! 1C exposes the remote-debug API below `/e1crdbg`. Keeping this module isolated
//! means DAP handling can be tested without a running 1C installation.

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fmt;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

const RDBG_REQUEST_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugRDBGRequestResponse";
const AUTO_ATTACH_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugAutoAttach";
const BREAKPOINTS_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugBreakpoints";
const DEBUG_BASE_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugBaseData";
const DEBUG_CALCULATIONS_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugCalculations";
const RTE_FILTER_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugRTEFilter";

/// A successfully registered 1C Debug UI session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugUiSession {
    id: String,
    info_base_alias: String,
}

/// A command delivered asynchronously by the 1C debug server to a Debug UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugUiEvent {
    pub command_id: String,
    pub target_id: Option<String>,
    pub call_stack: Vec<DebugStackFrame>,
    pub stopped_by_breakpoint: bool,
    pub suspended_by_other: bool,
    pub send_message_only: bool,
    pub send_hit_counter_only: bool,
    pub message: Option<String>,
    pub evaluation: Option<DebugEvaluation>,
}

/// A debuggable 1C execution context returned by `getDbgTargets`.
///
/// The identifier is stable for the lifetime of the context and is the value
/// accepted by `attachDetachDbgTargets`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugTarget {
    pub id: String,
    pub seance_no: String,
    pub user_name: String,
    pub target_type: String,
}

/// One call-stack item sent by RDBG as part of a `callStackFormed` event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugStackFrame {
    pub extension_name: String,
    pub object_id: String,
    pub property_id: String,
    pub line: i64,
    pub presentation: String,
}

/// A scalar result returned by RDBG expression evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugEvaluation {
    pub result_id: String,
    pub value: String,
    pub type_name: String,
    pub error: Option<String>,
    pub is_expandable: bool,
    pub is_indexed_collection: bool,
    pub variables: Vec<DebugVariable>,
}

/// A readable property in the local context of a stopped 1C stack frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugVariable {
    pub name: String,
    pub type_name: String,
    pub value: String,
    pub index: Option<i64>,
    pub is_expandable: bool,
    pub is_indexed_collection: bool,
}

/// An item in a calculation path used to obtain the children of a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalculationPathItem {
    Expression(String),
    Property(String),
    Index(i64),
}

/// Whether RDBG should describe an object context or an indexed collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationInterface {
    Context,
    Collection,
}

/// Immediate or deferred response to an expression request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationStart<T> {
    pub result_id: String,
    pub result: Option<T>,
}

/// Timing information for one RDBG long-poll. The adapter owns the trace
/// writer, so this small, owned value can safely cross back from its worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingTimings {
    pub http_elapsed: Duration,
    pub parse_elapsed: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingResult {
    pub events: Vec<DebugUiEvent>,
    pub timings: PingTimings,
}

/// Result of one Debug UI ping. A bounded ping is deliberately allowed to
/// expire without being treated as a connection failure: it is used only to
/// replace an older long-poll immediately after a resume command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingOutcome {
    Events(PingResult),
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepAction {
    Continue,
    Next,
    StepIn,
    StepOut,
}

/// One source breakpoint in the format understood by the 1C RDBG API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBreakpoint {
    pub line: i64,
    pub condition: Option<String>,
    pub hit_condition: Option<i64>,
    pub log_message: Option<String>,
}

/// The RDBG identity of a configuration module and its requested breakpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBreakpoints {
    pub extension_name: String,
    pub object_id: String,
    pub property_id: String,
    pub breakpoints: Vec<SourceBreakpoint>,
}

impl StepAction {
    fn rdbg_value(self) -> &'static str {
        match self {
            Self::Continue => "Continue",
            Self::Next => "Step",
            Self::StepIn => "StepIn",
            Self::StepOut => "StepOut",
        }
    }
}

impl DebugUiSession {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn info_base_alias(&self) -> &str {
        &self.info_base_alias
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachDebugUiResult {
    Registered,
    CredentialsRequired,
    FullCredentialsRequired,
    IbInDebug,
    NotRegistered,
    Unknown,
}

impl AttachDebugUiResult {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "registered" => Ok(Self::Registered),
            "credentialsRequired" => Ok(Self::CredentialsRequired),
            "fullCredentialsRequired" => Ok(Self::FullCredentialsRequired),
            "ibInDebug" => Ok(Self::IbInDebug),
            "notRegistered" => Ok(Self::NotRegistered),
            "unknown" => Ok(Self::Unknown),
            other => bail!("unknown attachDebugUI result `{other}`"),
        }
    }
}

impl fmt::Display for AttachDebugUiResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Registered => "registered",
            Self::CredentialsRequired => "credentialsRequired",
            Self::FullCredentialsRequired => "fullCredentialsRequired",
            Self::IbInDebug => "ibInDebug",
            Self::NotRegistered => "notRegistered",
            Self::Unknown => "unknown",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone)]
pub struct DebugServer {
    endpoint: String,
}

impl DebugServer {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        if host.trim().is_empty() {
            bail!("debugServerHost must not be empty");
        }
        Ok(Self {
            endpoint: format!("http://{host}:{port}/e1crdbg"),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Performs the same low-risk connectivity check used before attaching a UI.
    pub fn test_connection(&self) -> Result<()> {
        let url = format!("{}/rdbgTest?cmd=test", self.endpoint);
        let response = ureq::post(&url)
            .header("User-Agent", "1CV8")
            .header("Content-Type", "application/xml")
            .send_empty()
            .with_context(|| format!("cannot reach 1C debug server at {}", self.endpoint))?;
        let status = response.status();
        if !(200..300).contains(&status.as_u16()) {
            bail!("1C debug server returned HTTP {}", status);
        }
        Ok(())
    }

    /// Registers a Debug UI and returns its generated identifier.
    ///
    /// `info_base_alias` is the server-side infobase reference. For most
    /// server infobases it is the same value as the `Ref` connection-string
    /// attribute, rather than the display name in the 1C launcher.
    pub fn attach_debug_ui(&self, info_base_alias: &str) -> Result<DebugUiSession> {
        if info_base_alias.trim().is_empty() {
            bail!("infoBaseAlias must not be empty");
        }

        self.test_connection()?;
        let session = DebugUiSession {
            id: Uuid::new_v4().to_string(),
            info_base_alias: info_base_alias.to_owned(),
        };
        let body = attach_debug_ui_request(&session);
        let response = self.post_xml("attachDebugUI", &body)?;
        let result = AttachDebugUiResult::parse(&response_element(&response, "result")?)?;

        match result {
            AttachDebugUiResult::Registered => Ok(session),
            AttachDebugUiResult::CredentialsRequired
            | AttachDebugUiResult::FullCredentialsRequired => {
                bail!("1C debug server requires infobase credentials")
            }
            AttachDebugUiResult::IbInDebug => bail!("the infobase is already being debugged"),
            AttachDebugUiResult::NotRegistered => {
                bail!("the infobase is not registered for debugging")
            }
            AttachDebugUiResult::Unknown => {
                bail!("the 1C debug server returned an unknown attach result")
            }
        }
    }

    /// Unregisters the supplied Debug UI.
    ///
    /// Some dbgs builds return an empty or false XML result after successfully
    /// processing the request, so HTTP success is the reliable acknowledgement.
    pub fn detach_debug_ui(&self, session: &DebugUiSession) -> Result<()> {
        let body = base_request(session);
        self.post_xml("detachDebugUI", &body).map(|_| ())
    }

    /// Configures which 1C execution contexts are automatically attached to
    /// this Debug UI. The spellings intentionally match `autoAttachTypes` in
    /// the existing VS Code extension.
    pub fn set_auto_attach_types(&self, session: &DebugUiSession, types: &[String]) -> Result<()> {
        for target_type in types {
            debug_target_type_xml_value(target_type)?;
        }
        let body = auto_attach_settings_request(session, types);
        self.post_xml("setAutoAttachSettings", &body).map(|_| ())
    }

    /// Lists execution contexts that can be manually attached from VS Code.
    pub fn get_debug_targets(&self, session: &DebugUiSession) -> Result<Vec<DebugTarget>> {
        let response = self.post_xml("getDbgTargets", &base_request(session))?;
        parse_debug_targets(&response)
    }

    /// Fetches pending Debug UI commands and separates network wait from XML
    /// parsing so latency traces can identify the source of a slow step.
    pub fn ping_debug_ui_timed(
        &self,
        session: &DebugUiSession,
        receive_timeout: Option<Duration>,
    ) -> Result<PingOutcome> {
        let url = format!(
            "{}/rdbg?cmd=pingDebugUIParams&dbgui={}",
            self.endpoint,
            session.id()
        );
        let http_started = Instant::now();
        let response =
            match self.post_empty_with_timeout(&url, "pingDebugUIParams", receive_timeout) {
                Ok(response) => response,
                Err(PingRequestError::TimedOut) => return Ok(PingOutcome::TimedOut),
                Err(PingRequestError::Failed(error)) => return Err(error),
            };
        let http_elapsed = http_started.elapsed();
        let parse_started = Instant::now();
        let events = parse_debug_ui_events(&response)?;
        Ok(PingOutcome::Events(PingResult {
            events,
            timings: PingTimings {
                http_elapsed,
                parse_elapsed: parse_started.elapsed(),
            },
        }))
    }

    /// Attaches the Debug UI to the supplied execution contexts.
    pub fn attach_debug_targets(
        &self,
        session: &DebugUiSession,
        target_ids: &[String],
    ) -> Result<()> {
        if target_ids.is_empty() {
            return Ok(());
        }
        let body = debug_target_request(session, true, target_ids);
        self.post_xml("attachDetachDbgTargets", &body).map(|_| ())
    }

    /// Clears a pending global "break on next statement" request before a
    /// newly announced execution context is attached. Otherwise RDBG can stop
    /// the context immediately for a pause request that VS Code has already
    /// completed.
    pub fn clear_break_on_next_statement(&self, session: &DebugUiSession) -> Result<()> {
        self.post_xml("clearBreakOnNextStatement", &base_request(session))
            .map(|_| ())
    }

    /// Continues or steps a single 1C execution context.
    pub fn step(
        &self,
        session: &DebugUiSession,
        target_id: &str,
        action: StepAction,
    ) -> Result<()> {
        let body = step_request(session, target_id, action);
        self.post_xml("step", &body).map(|_| ())
    }

    /// Requests a pause at the next executable statement in the infobase.
    pub fn break_on_next_statement(&self, session: &DebugUiSession) -> Result<()> {
        self.post_xml("setBreakOnNextStatement", &base_request(session))
            .map(|_| ())
    }

    /// Replaces the complete internal breakpoint workspace for this Debug UI.
    ///
    /// RDBG does not offer a per-file mutation. Callers should retain all files'
    /// breakpoints and submit the complete workspace whenever VS Code changes one.
    pub fn set_breakpoints(
        &self,
        session: &DebugUiSession,
        modules: &[ModuleBreakpoints],
    ) -> Result<()> {
        self.post_xml("setBreakpoints", &breakpoints_request(session, modules))
            .map(|_| ())
    }

    /// Enables or disables pauses on 1C runtime errors. When a template is
    /// supplied, RDBG evaluates it against the error presentation.
    pub fn set_runtime_error_processing(
        &self,
        session: &DebugUiSession,
        stop_on_errors: bool,
        error_template: Option<&str>,
    ) -> Result<()> {
        self.post_xml(
            "setBreakOnRTE",
            &runtime_error_processing_request(session, stop_on_errors, error_template),
        )
        .map(|_| ())
    }

    /// Starts evaluating an expression in a stopped target stack frame.
    ///
    /// RDBG normally answers in this HTTP response, but older servers may
    /// defer it and deliver `exprEvaluated` from `pingDebugUIParams` instead.
    pub fn begin_evaluate_expression(
        &self,
        session: &DebugUiSession,
        target_id: &str,
        stack_level: i64,
        expression: &str,
    ) -> Result<EvaluationStart<DebugEvaluation>> {
        if expression.trim().is_empty() {
            bail!("expression must not be empty");
        }
        self.begin_evaluate_path(
            session,
            target_id,
            stack_level,
            &[CalculationPathItem::Expression(expression.to_owned())],
            EvaluationInterface::Context,
        )
    }

    /// Starts evaluating a path to obtain the children of an expandable value.
    pub fn begin_evaluate_path(
        &self,
        session: &DebugUiSession,
        target_id: &str,
        stack_level: i64,
        path: &[CalculationPathItem],
        interface: EvaluationInterface,
    ) -> Result<EvaluationStart<DebugEvaluation>> {
        if path.is_empty() {
            bail!("calculation path must not be empty");
        }
        let result_id = Uuid::new_v4().to_string();
        let body = evaluation_request(session, target_id, stack_level, &result_id, path, interface);
        let response = self.post_xml("evalExpr", &body)?;
        Ok(EvaluationStart {
            result_id,
            result: parse_evaluation_response(&response)?,
        })
    }

    /// Starts retrieving readable local variables for a stopped target stack
    /// frame. The result can likewise be delivered by `exprEvaluated`.
    pub fn begin_evaluate_local_variables(
        &self,
        session: &DebugUiSession,
        target_id: &str,
        stack_level: i64,
    ) -> Result<EvaluationStart<Vec<DebugVariable>>> {
        let result_id = Uuid::new_v4().to_string();
        let body = local_variables_request(session, target_id, stack_level, &result_id);
        let response = self.post_xml("evalLocalVariables", &body)?;
        Ok(EvaluationStart {
            result_id,
            result: parse_local_variables_response(&response)?,
        })
    }

    fn post_xml(&self, command: &str, body: &str) -> Result<String> {
        let url = format!("{}/rdbg?cmd={command}", self.endpoint);
        let mut response = ureq::post(&url)
            .header("User-Agent", "1CV8")
            .header("Content-Type", "application/xml; charset=utf-8")
            .send(body)
            .with_context(|| format!("1C debug server request `{command}` failed"))?;
        response
            .body_mut()
            .read_to_string()
            .context("cannot read XML response from 1C debug server")
    }

    fn post_empty_with_timeout(
        &self,
        url: &str,
        command: &str,
        receive_timeout: Option<Duration>,
    ) -> std::result::Result<String, PingRequestError> {
        let response = match receive_timeout {
            Some(timeout) => ureq::post(url)
                .header("User-Agent", "1CV8")
                .config()
                .timeout_global(Some(timeout))
                .build()
                .send_empty(),
            None => ureq::post(url).header("User-Agent", "1CV8").send_empty(),
        };
        let mut response = match response {
            Ok(response) => response,
            Err(ureq::Error::Timeout(_)) => return Err(PingRequestError::TimedOut),
            Err(error) => {
                return Err(PingRequestError::Failed(
                    anyhow::Error::from(error)
                        .context(format!("1C debug server request `{command}` failed")),
                ));
            }
        };
        response
            .body_mut()
            .read_to_string()
            .context("cannot read XML response from 1C debug server")
            .map_err(PingRequestError::Failed)
    }
}

enum PingRequestError {
    TimedOut,
    Failed(anyhow::Error),
}

fn attach_debug_ui_request(session: &DebugUiSession) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><request xmlns=\"{RDBG_REQUEST_NAMESPACE}\"><infoBaseAlias>{}</infoBaseAlias><idOfDebuggerUI>{}</idOfDebuggerUI><userName></userName><credentials></credentials><options><foregroundAbility>true</foregroundAbility></options></request>",
        xml_escape(session.info_base_alias()),
        xml_escape(session.id())
    )
}

fn base_request(session: &DebugUiSession) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><request xmlns=\"{RDBG_REQUEST_NAMESPACE}\"><infoBaseAlias>{}</infoBaseAlias><idOfDebuggerUI>{}</idOfDebuggerUI></request>",
        xml_escape(session.info_base_alias()),
        xml_escape(session.id())
    )
}

fn auto_attach_settings_request(session: &DebugUiSession, types: &[String]) -> String {
    let target_types = types
        .iter()
        .map(|target_type| {
            format!(
                "<targetType xmlns=\"{AUTO_ATTACH_NAMESPACE}\">{}</targetType>",
                debug_target_type_xml_value(target_type).expect("validated before serialization")
            )
        })
        .collect::<String>();
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!("<autoAttachSettings>{target_types}</autoAttachSettings></request>"),
        1,
    )
}

fn debug_target_request(session: &DebugUiSession, attach: bool, target_ids: &[String]) -> String {
    let targets = target_ids
        .iter()
        .map(|target_id| {
            format!(
                "<id><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</id></id>",
                xml_escape(target_id)
            )
        })
        .collect::<String>();
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!("<attach>{attach}</attach>{targets}</request>"),
        1,
    )
}

fn step_request(session: &DebugUiSession, target_id: &str, action: StepAction) -> String {
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!(
            "<targetID><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</id></targetID><action>{}</action><simple>false</simple></request>",
            xml_escape(target_id),
            action.rdbg_value()
        ),
        1,
    )
}

fn breakpoints_request(session: &DebugUiSession, modules: &[ModuleBreakpoints]) -> String {
    let workspace = modules
        .iter()
        .map(|module| {
            let module_type = if module.extension_name.is_empty() {
                "ConfigModule"
            } else {
                "ExtensionModule"
            };
            let breakpoints = module
                .breakpoints
                .iter()
                .map(|breakpoint| {
                    let condition = breakpoint.condition.as_deref().unwrap_or_default();
                    let log_message = breakpoint.log_message.as_deref().unwrap_or_default();
                    let hit_count = breakpoint.hit_condition.unwrap_or(1);
                    let is_conditional = breakpoint.condition.is_some();
                    let has_hit_count = breakpoint.hit_condition.is_some();
                    let is_log_point = breakpoint.log_message.is_some();
                    format!(
                        "<bpInfo><line>{}</line><isActive>true</isActive><breakOnCondition>{is_conditional}</breakOnCondition><condition>{}</condition><breakOnParentMethod>false</breakOnParentMethod><parentMethod></parentMethod><breakOnHitCount>{has_hit_count}</breakOnHitCount><hitCountVariant>0</hitCountVariant><hitCount>{hit_count}</hitCount><showOutputMessage>{is_log_point}</showOutputMessage><putDescription></putDescription><putExpressionResult>{}</putExpressionResult><putStackTrace>false</putStackTrace><putHitCount>false</putHitCount><continueExecution>{is_log_point}</continueExecution><currentHitCounter>0</currentHitCounter><temp>false</temp><user>true</user></bpInfo>",
                        breakpoint.line,
                        xml_escape(condition),
                        xml_escape(log_message),
                    )
                })
                .collect::<String>();
            format!(
                "<moduleBPInfo xmlns=\"{BREAKPOINTS_NAMESPACE}\"><id><type xmlns=\"{DEBUG_BASE_NAMESPACE}\">{module_type}</type><URL xmlns=\"{DEBUG_BASE_NAMESPACE}\"></URL><extensionName xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</extensionName><objectID xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</objectID><propertyID xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</propertyID><extId xmlns=\"{DEBUG_BASE_NAMESPACE}\">0</extId></id>{breakpoints}</moduleBPInfo>",
                xml_escape(&module.extension_name),
                xml_escape(&module.object_id),
                xml_escape(&module.property_id),
            )
        })
        .collect::<String>();
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!("<bpWorkspace>{workspace}</bpWorkspace></request>"),
        1,
    )
}

fn runtime_error_processing_request(
    session: &DebugUiSession,
    stop_on_errors: bool,
    error_template: Option<&str>,
) -> String {
    let analyze_error = error_template.is_some_and(|template| !template.is_empty());
    let template = error_template
        .filter(|template| !template.is_empty())
        .map(|template| {
            format!(
                "<strTemplate><include>true</include><str>{}</str></strTemplate>",
                xml_escape(template)
            )
        })
        .unwrap_or_default();
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!(
            "<state xmlns=\"{RTE_FILTER_NAMESPACE}\"><stopOnErrors>{stop_on_errors}</stopOnErrors><analyzeErrorStr>{analyze_error}</analyzeErrorStr>{template}</state></request>"
        ),
        1,
    )
}

fn evaluation_request(
    session: &DebugUiSession,
    target_id: &str,
    stack_level: i64,
    result_id: &str,
    path: &[CalculationPathItem],
    interface: EvaluationInterface,
) -> String {
    let items = path
        .iter()
        .map(|item| match item {
            CalculationPathItem::Expression(expression) => format!(
                "<calcItem><itemType>expression</itemType><expression>{}</expression><property></property></calcItem>",
                xml_escape(expression)
            ),
            CalculationPathItem::Property(property) => format!(
                "<calcItem><itemType>property</itemType><expression></expression><property>{}</property></calcItem>",
                xml_escape(property)
            ),
            CalculationPathItem::Index(index) => format!(
                "<calcItem><itemType>index</itemType><expression></expression><property></property><index>{index}</index></calcItem>"
            ),
        })
        .collect::<String>();
    let interface = match interface {
        EvaluationInterface::Context => "context",
        EvaluationInterface::Collection => "collection",
    };
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!(
            "<calcWaitingTime>100</calcWaitingTime><targetID><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</id></targetID><expr><stackLevel xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\">{}</stackLevel><srcCalcInfo xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\"><expressionResultID>{result_id}</expressionResultID>{items}<interfaces>{interface}</interfaces></srcCalcInfo><presOptions xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\"><maxTextSize>307200</maxTextSize><stopOnFirstEOL>false</stopOnFirstEOL></presOptions></expr></request>",
            xml_escape(target_id),
            stack_level.max(0),
        ),
        1,
    )
}

fn local_variables_request(
    session: &DebugUiSession,
    target_id: &str,
    stack_level: i64,
    result_id: &str,
) -> String {
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!(
            "<calcWaitingTime>100</calcWaitingTime><targetID><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">{}</id></targetID><expr><stackLevel xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\">{}</stackLevel><srcCalcInfo xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\"><expressionResultID>{result_id}</expressionResultID><interfaces>context</interfaces></srcCalcInfo><presOptions xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\"><maxTextSize>307200</maxTextSize><stopOnFirstEOL>false</stopOnFirstEOL></presOptions></expr></request>",
            xml_escape(target_id),
            stack_level.max(0),
        ),
        1,
    )
}

fn parse_evaluation_response(xml: &str) -> Result<Option<DebugEvaluation>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut names = Vec::<String>::new();
    let mut saw_result = false;
    let mut result_id = String::new();
    let mut error_occurred = false;
    let mut value = None;
    let mut type_name = None;
    let mut error = None;
    let mut is_expandable = false;
    let mut is_indexed_collection = false;

    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "result" {
                    saw_result = true;
                }
                names.push(name);
            }
            Event::End(_) => {
                names.pop();
            }
            Event::Text(text) => {
                let Some(name) = names.last().map(String::as_str) else {
                    continue;
                };
                let text = text
                    .unescape()
                    .map(|text| text.into_owned())
                    .context("cannot decode expression evaluation response")?;
                let result_value = names.iter().any(|entry| entry == "resultValueInfo");
                match name {
                    "expressionResultID" => result_id = text,
                    "errorOccurred" => error_occurred = xml_bool(&text),
                    "typeName" if result_value => type_name = Some(text),
                    "pres" if result_value => value = Some(decode_base64_text(&text)),
                    "exceptionStr" => error = Some(decode_base64_text(&text)),
                    "isExpandable" if result_value => is_expandable = xml_bool(&text),
                    "isIIndexedCollectionRO" if result_value => {
                        is_indexed_collection = xml_bool(&text)
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !saw_result {
        return Ok(None);
    }
    Ok(Some(DebugEvaluation {
        result_id,
        value: value.unwrap_or_default(),
        type_name: type_name.unwrap_or_default(),
        error: error_occurred
            .then_some(error.unwrap_or_else(|| "expression evaluation failed".to_owned())),
        is_expandable,
        is_indexed_collection,
        variables: parse_context_variables(xml)?,
    }))
}

fn parse_local_variables_response(xml: &str) -> Result<Option<Vec<DebugVariable>>> {
    let saw_result = xml.contains("<result") || xml.contains(":result");
    saw_result.then(|| parse_context_variables(xml)).transpose()
}

fn parse_context_variables(xml: &str) -> Result<Vec<DebugVariable>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut names = Vec::<String>::new();
    let mut current = None;
    let mut variables = Vec::new();
    let mut collection_index = 0_i64;

    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "valueOfContextPropInfo" {
                    current = Some(DebugVariable::default());
                } else if name == "valueOfCollectionInfo" {
                    current = Some(DebugVariable {
                        name: collection_index.to_string(),
                        index: Some(collection_index),
                        ..DebugVariable::default()
                    });
                    collection_index += 1;
                }
                names.push(name);
            }
            Event::End(element) => {
                if matches!(
                    element.local_name().as_ref(),
                    b"valueOfContextPropInfo" | b"valueOfCollectionInfo"
                ) {
                    if let Some(variable) = current.take() {
                        if !variable.name.is_empty() {
                            variables.push(variable);
                        }
                    }
                }
                names.pop();
            }
            Event::Text(text) => {
                let Some(variable) = &mut current else {
                    continue;
                };
                let Some(name) = names.last().map(String::as_str) else {
                    continue;
                };
                let text = text
                    .unescape()
                    .map(|text| text.into_owned())
                    .context("cannot decode local variables response")?;
                match name {
                    "propName" => variable.name = text,
                    "typeName" => variable.type_name = text,
                    "pres" | "errorStr" => variable.value = decode_base64_text(&text),
                    "isExpandable" => variable.is_expandable = xml_bool(&text),
                    "isIIndexedCollectionRO" => variable.is_indexed_collection = xml_bool(&text),
                    _ => {}
                }
            }
            Event::Eof => return Ok(variables),
            _ => {}
        }
    }
}

fn decode_base64_text(value: &str) -> String {
    BASE64
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| value.to_owned())
}

fn debug_target_type_xml_value(target_type: &str) -> Result<&'static str> {
    match target_type {
        "Client" => Ok("Client"),
        "ManagedClient" => Ok("ManagedClient"),
        "Server" => Ok("Server"),
        "ServerEmulation" => Ok("ServerEmulation"),
        "OData" => Ok("OData"),
        "JobFileMode" => Ok("JobFileMode"),
        "MobileClient" => Ok("MobileClient"),
        "MobileServer" => Ok("MobileServer"),
        "MobileJobFileMode" => Ok("MobileJobFileMode"),
        "MobileManagedClient" => Ok("MobileManagedClient"),
        "MobileManagedServer" => Ok("MobileManagedServer"),
        "WebClient" => Ok("WEBClient"),
        "ComConnector" => Ok("COMConnector"),
        "WebService" => Ok("WEBService"),
        "HttpService" => Ok("HTTPService"),
        "Job" => Ok("JOB"),
        other => bail!("unsupported autoAttachTypes value `{other}`"),
    }
}

fn response_element(xml: &str, expected_element: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event()? {
            Event::Start(element)
                if element.local_name().as_ref() == expected_element.as_bytes() =>
            {
                return reader
                    .read_text(element.name())
                    .map(|text| text.into_owned())
                    .context("cannot read XML response value");
            }
            Event::Eof => bail!("1C debug server response has no `{expected_element}` element"),
            _ => {}
        }
    }
}

fn parse_debug_targets(xml: &str) -> Result<Vec<DebugTarget>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut names = Vec::<String>::new();
    let mut current = None::<DebugTarget>;
    let mut targets = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                // RDBGSGetDbgTargetsResponse is a flat `<response><id>…`
                // collection. Its inner `id` is the target UUID.
                if name == "id" && names.len() == 1 && names[0] == "response" {
                    current = Some(DebugTarget::default());
                }
                names.push(name);
            }
            Event::End(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "id" && names.len() == 2 && names[0] == "response" {
                    if let Some(target) = current.take().filter(|target| !target.id.is_empty()) {
                        targets.push(target);
                    }
                }
                names.pop();
            }
            Event::Text(text) => {
                let Some(target) = &mut current else {
                    continue;
                };
                let value = text
                    .unescape()
                    .map(|value| value.into_owned())
                    .context("cannot decode debug targets response")?;
                match names.last().map(String::as_str) {
                    Some("id") if names.len() == 3 => target.id = value,
                    Some("seanceNo") => target.seance_no = value,
                    Some("userName") => target.user_name = value,
                    Some("targetType") => target.target_type = value,
                    _ => {}
                }
            }
            Event::Eof => return Ok(targets),
            _ => {}
        }
    }
}

fn parse_debug_ui_events(xml: &str) -> Result<Vec<DebugUiEvent>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut depth = 0usize;
    let mut names = Vec::<String>::new();
    let mut current_event = None;
    let mut current_stack_frame = None;
    let mut current_evaluation_variable = None;
    let mut evaluation_collection_index = 0_i64;
    let mut events = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                depth += 1;
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "result" && depth == 2 {
                    current_event = Some(DebugUiEvent::default());
                } else if current_event.is_some() && name == "callStack" {
                    current_stack_frame = Some(DebugStackFrame::default());
                }
                if current_stack_frame.is_none() && name == "valueOfContextPropInfo" {
                    current_evaluation_variable = Some(DebugVariable::default());
                } else if current_stack_frame.is_none() && name == "valueOfCollectionInfo" {
                    current_evaluation_variable = Some(DebugVariable {
                        name: evaluation_collection_index.to_string(),
                        index: Some(evaluation_collection_index),
                        ..DebugVariable::default()
                    });
                    evaluation_collection_index += 1;
                }
                names.push(name);
            }
            Event::End(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "callStack" {
                    if let (Some(event), Some(frame)) =
                        (&mut current_event, current_stack_frame.take())
                    {
                        event.call_stack.push(frame);
                    }
                }
                if matches!(
                    name.as_str(),
                    "valueOfContextPropInfo" | "valueOfCollectionInfo"
                ) {
                    if let (Some(event), Some(variable)) =
                        (&mut current_event, current_evaluation_variable.take())
                    {
                        if let Some(evaluation) = &mut event.evaluation {
                            if !variable.name.is_empty() {
                                evaluation.variables.push(variable);
                            }
                        }
                    }
                }
                if name == "result" && depth == 2 {
                    if let Some(event) = current_event.take() {
                        if !event.command_id.is_empty() {
                            events.push(event);
                        }
                    }
                }
                names.pop();
                depth = depth.saturating_sub(1);
            }
            Event::Text(text) => {
                let Some(event) = &mut current_event else {
                    continue;
                };
                let value = text
                    .unescape()
                    .map(|value| value.into_owned())
                    .context("cannot decode Debug UI event value")?;
                let Some(name) = names.last().map(String::as_str) else {
                    continue;
                };
                match name {
                    "cmdID" => {
                        event.command_id = value.clone();
                        if event.command_id == "exprEvaluated" {
                            event.evaluation = Some(DebugEvaluation::default());
                        }
                    }
                    "id" if names.iter().any(|name| name == "targetID") => {
                        event.target_id = Some(value.clone())
                    }
                    "stopByBP" => event.stopped_by_breakpoint = xml_bool(&value),
                    "suspendedByOther" => event.suspended_by_other = xml_bool(&value),
                    "sendMessageOnly" => event.send_message_only = xml_bool(&value),
                    "sendHitCounterOnly" => event.send_hit_counter_only = xml_bool(&value),
                    "message" => event.message = Some(value.clone()),
                    "extensionName" => set_stack_field(&mut current_stack_frame, |frame| {
                        frame.extension_name = value.clone()
                    }),
                    "objectID" => set_stack_field(&mut current_stack_frame, |frame| {
                        frame.object_id = value.clone()
                    }),
                    "propertyID" => set_stack_field(&mut current_stack_frame, |frame| {
                        frame.property_id = value.clone()
                    }),
                    "lineNo" => set_stack_field(&mut current_stack_frame, |frame| {
                        frame.line = value.parse().unwrap_or_default()
                    }),
                    "presentation" => set_stack_field(&mut current_stack_frame, |frame| {
                        frame.presentation = BASE64
                            .decode(value.as_bytes())
                            .ok()
                            .and_then(|bytes| String::from_utf8(bytes).ok())
                            .unwrap_or_else(|| value.clone())
                    }),
                    _ => {}
                }
                if current_stack_frame.is_none() {
                    if let Some(variable) = &mut current_evaluation_variable {
                        match name {
                            "propName" => variable.name = value.clone(),
                            "typeName" => variable.type_name = value.clone(),
                            "pres" | "errorStr" => variable.value = decode_base64_text(&value),
                            "isExpandable" => variable.is_expandable = xml_bool(&value),
                            "isIIndexedCollectionRO" => {
                                variable.is_indexed_collection = xml_bool(&value)
                            }
                            _ => {}
                        }
                    }
                    if let Some(evaluation) = &mut event.evaluation {
                        let result_value = names.iter().any(|entry| entry == "resultValueInfo");
                        match name {
                            "expressionResultID" => evaluation.result_id = value,
                            "errorOccurred" => error_flag(&mut evaluation.error, &value),
                            "typeName" if result_value => evaluation.type_name = value,
                            "pres" if result_value => evaluation.value = decode_base64_text(&value),
                            "exceptionStr" => evaluation.error = Some(decode_base64_text(&value)),
                            "isExpandable" if result_value => {
                                evaluation.is_expandable = xml_bool(&value)
                            }
                            "isIIndexedCollectionRO" if result_value => {
                                evaluation.is_indexed_collection = xml_bool(&value)
                            }
                            _ => {}
                        }
                    }
                }
            }
            Event::Eof => return Ok(events),
            _ => {}
        }
    }
}

fn set_stack_field(frame: &mut Option<DebugStackFrame>, set: impl FnOnce(&mut DebugStackFrame)) {
    if let Some(frame) = frame {
        set(frame);
    }
}

fn xml_bool(value: &str) -> bool {
    value == "true" || value == "1"
}

fn error_flag(error: &mut Option<String>, value: &str) {
    if xml_bool(value) && error.is_none() {
        *error = Some("expression evaluation failed".to_owned());
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn attach_request_contains_escaped_base_request_data() {
        let session = DebugUiSession {
            id: "0f45f589-6c7d-4f5f-8433-4e8f611e6a9a".to_owned(),
            info_base_alias: "Sales & <Retail>".to_owned(),
        };

        let xml = attach_debug_ui_request(&session);

        assert!(xml.contains("<infoBaseAlias>Sales &amp; &lt;Retail&gt;</infoBaseAlias>"));
        assert!(
            xml.contains("<idOfDebuggerUI>0f45f589-6c7d-4f5f-8433-4e8f611e6a9a</idOfDebuggerUI>")
        );
        assert!(xml.contains("<foregroundAbility>true</foregroundAbility>"));
    }

    #[test]
    fn parses_namespaced_response_result() {
        let xml = "<response xmlns=\"http://v8.1c.ru/8.3/debugger/debugBaseData\"><result>registered</result></response>";
        assert_eq!(response_element(xml, "result").unwrap(), "registered");
    }

    #[test]
    fn recognizes_all_documented_attach_results() {
        assert_eq!(
            AttachDebugUiResult::parse("registered").unwrap(),
            AttachDebugUiResult::Registered
        );
        assert_eq!(
            AttachDebugUiResult::parse("ibInDebug").unwrap(),
            AttachDebugUiResult::IbInDebug
        );
        assert!(AttachDebugUiResult::parse("unexpected").is_err());
    }

    #[test]
    fn converts_vscode_auto_attach_types_to_rdbg_values() {
        assert_eq!(
            debug_target_type_xml_value("HttpService").unwrap(),
            "HTTPService"
        );
        assert_eq!(
            debug_target_type_xml_value("WebClient").unwrap(),
            "WEBClient"
        );
        assert!(debug_target_type_xml_value("Unknown").is_err());
    }

    #[test]
    fn parses_debug_targets_returned_by_rdbg() {
        let targets = parse_debug_targets(
            "<response xmlns=\"http://v8.1c.ru/8.3/debugger/debugRDBGRequestResponse\"><id><id>target-1</id><seanceNo>17</seanceNo><userName>Администратор</userName><targetType>ManagedClient</targetType></id><id><id>target-2</id><seanceNo>18</seanceNo><userName></userName><targetType>HTTPService</targetType></id></response>",
        )
        .unwrap();

        assert_eq!(
            targets,
            vec![
                DebugTarget {
                    id: "target-1".to_owned(),
                    seance_no: "17".to_owned(),
                    user_name: "Администратор".to_owned(),
                    target_type: "ManagedClient".to_owned(),
                },
                DebugTarget {
                    id: "target-2".to_owned(),
                    seance_no: "18".to_owned(),
                    user_name: String::new(),
                    target_type: "HTTPService".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn parses_target_lifecycle_events() {
        let events = parse_debug_ui_events(
            "<response><result><cmdID>targetStarted</cmdID><targetID><id>target-1</id></targetID></result><result><cmdID>targetQuit</cmdID><targetID><id>target-1</id></targetID></result></response>",
        )
        .unwrap();

        assert_eq!(
            events,
            vec![
                DebugUiEvent {
                    command_id: "targetStarted".to_owned(),
                    target_id: Some("target-1".to_owned()),
                    ..DebugUiEvent::default()
                },
                DebugUiEvent {
                    command_id: "targetQuit".to_owned(),
                    target_id: Some("target-1".to_owned()),
                    ..DebugUiEvent::default()
                },
            ]
        );
    }

    #[test]
    fn parses_call_stack_events() {
        let events = parse_debug_ui_events(
            "<response><result><cmdID>callStackFormed</cmdID><targetID><id>target-1</id></targetID><stopByBP>true</stopByBP><callStack><moduleID><extensionName></extensionName><objectID>object-id</objectID><propertyID>property-id</propertyID></moduleID><lineNo>42</lineNo><presentation>VGVzdE1ldGhvZA==</presentation></callStack></result></response>",
        )
        .unwrap();

        assert_eq!(events[0].target_id.as_deref(), Some("target-1"));
        assert!(events[0].stopped_by_breakpoint);
        assert_eq!(events[0].call_stack[0].line, 42);
        assert_eq!(events[0].call_stack[0].presentation, "TestMethod");
    }

    #[test]
    fn serializes_step_requests_with_the_rdbg_action_name() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = step_request(&session, "target-1", StepAction::StepIn);

        assert!(xml.contains(&format!(
            "<targetID><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">target-1</id></targetID>"
        )));
        assert!(xml.contains("<action>StepIn</action>"));
    }

    #[test]
    fn serializes_conditional_and_log_breakpoints() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = breakpoints_request(
            &session,
            &[ModuleBreakpoints {
                extension_name: String::new(),
                object_id: "object-id".to_owned(),
                property_id: "property-id".to_owned(),
                breakpoints: vec![SourceBreakpoint {
                    line: 42,
                    condition: Some("A < 3".to_owned()),
                    hit_condition: Some(5),
                    log_message: Some("A={A}".to_owned()),
                }],
            }],
        );

        assert!(xml.contains("<bpWorkspace>"));
        assert!(xml.contains(&format!(
            "<moduleBPInfo xmlns=\"{BREAKPOINTS_NAMESPACE}\"><id><type xmlns=\"{DEBUG_BASE_NAMESPACE}\">ConfigModule</type>"
        )));
        assert!(!xml.contains("<version>"));
        assert!(xml.contains("<line>42</line>"));
        assert!(xml.contains("<condition>A &lt; 3</condition>"));
        assert!(xml.contains("<hitCount>5</hitCount>"));
        assert!(xml.contains("<putExpressionResult>A={A}</putExpressionResult>"));
        assert!(xml.contains("<continueExecution>true</continueExecution>"));
    }

    #[test]
    fn serializes_empty_auto_attach_settings() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };

        let xml = auto_attach_settings_request(&session, &[]);

        assert!(xml.contains("<autoAttachSettings></autoAttachSettings>"));
    }

    #[test]
    fn serializes_auto_attach_target_types_in_the_schema_namespace() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };

        let xml = auto_attach_settings_request(&session, &["ManagedClient".to_owned()]);

        assert!(xml.contains(&format!(
            "<targetType xmlns=\"{AUTO_ATTACH_NAMESPACE}\">ManagedClient</targetType>"
        )));
    }

    #[test]
    fn serializes_runtime_error_filter() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = runtime_error_processing_request(&session, true, Some("division by zero"));

        assert!(xml.contains(&format!(
            "<state xmlns=\"{RTE_FILTER_NAMESPACE}\"><stopOnErrors>true</stopOnErrors>"
        )));
        assert!(xml.contains("<analyzeErrorStr>true</analyzeErrorStr>"));
        assert!(xml.contains("<str>division by zero</str>"));
    }

    #[test]
    fn serializes_and_parses_expression_evaluation() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = evaluation_request(
            &session,
            "target-1",
            2,
            "request-id",
            &[CalculationPathItem::Expression("A < 3".to_owned())],
            EvaluationInterface::Context,
        );
        assert!(xml.contains("<calcWaitingTime>100</calcWaitingTime>"));
        assert!(xml.contains(&format!(
            "<targetID><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">target-1</id></targetID>"
        )));
        assert!(xml.contains(&format!(
            "<stackLevel xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\">2</stackLevel>"
        )));
        assert!(xml.contains("<expression>A &lt; 3</expression>"));

        let result = parse_evaluation_response(
            "<response><result><errorOccurred>false</errorOccurred><resultValueInfo><typeName>Boolean</typeName><pres>0JjRgdGC0LjQvdCw</pres></resultValueInfo></result></response>",
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.type_name, "Boolean");
        assert_eq!(result.value, "Истина");
        assert_eq!(result.error, None);
    }

    #[test]
    fn supports_deferred_expression_evaluation_events() {
        assert_eq!(
            parse_evaluation_response("<response></response>").unwrap(),
            None
        );

        let events = parse_debug_ui_events(
            "<response><result><cmdID>exprEvaluated</cmdID><evalExprResBaseData><expressionResultID>result-1</expressionResultID><errorOccurred>false</errorOccurred><resultValueInfo><typeName>Structure</typeName><pres>VGVzdA==</pres><isExpandable>true</isExpandable><isIIndexedCollectionRO>false</isIIndexedCollectionRO></resultValueInfo><calculationResult><valueOfContextPropInfo><propInfo><propName>Counter</propName></propInfo><valueInfo><typeName>Number</typeName><pres>NDI=</pres></valueInfo></valueOfContextPropInfo></calculationResult></evalExprResBaseData></result></response>",
        )
        .unwrap();

        let evaluation = events[0].evaluation.as_ref().unwrap();
        assert_eq!(evaluation.result_id, "result-1");
        assert_eq!(evaluation.value, "Test");
        assert!(evaluation.is_expandable);
        assert_eq!(evaluation.variables[0].name, "Counter");
        assert_eq!(evaluation.variables[0].value, "42");
    }

    #[test]
    fn serializes_property_and_index_calculation_paths() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = evaluation_request(
            &session,
            "target-1",
            0,
            "request-id",
            &[
                CalculationPathItem::Expression("Items".to_owned()),
                CalculationPathItem::Property("Owner".to_owned()),
                CalculationPathItem::Index(3),
            ],
            EvaluationInterface::Collection,
        );

        assert!(xml.contains(
            "<itemType>property</itemType><expression></expression><property>Owner</property>"
        ));
        assert!(xml.contains("<itemType>index</itemType><expression></expression><property></property><index>3</index>"));
        assert!(xml.contains("<interfaces>collection</interfaces>"));
    }

    #[test]
    fn parses_expandable_context_and_collection_values() {
        let variables = parse_context_variables(
            "<response><result><calculationResult><valueOfContextPropInfo><propInfo><propName>Document</propName></propInfo><valueInfo><typeName>DocumentObject</typeName><pres>RGVtbw==</pres><isExpandable>true</isExpandable></valueInfo></valueOfContextPropInfo><valueOfCollectionInfo><valueInfo><typeName>String</typeName><pres>Zmlyc3Q=</pres><isIIndexedCollectionRO>true</isIIndexedCollectionRO></valueInfo></valueOfCollectionInfo></calculationResult></result></response>",
        )
        .unwrap();

        assert!(variables[0].is_expandable);
        assert_eq!(variables[0].value, "Demo");
        assert_eq!(variables[1].index, Some(0));
        assert!(variables[1].is_indexed_collection);
        assert_eq!(variables[1].value, "first");
    }

    #[test]
    fn serializes_and_parses_local_variables() {
        let session = DebugUiSession {
            id: "debug-ui".to_owned(),
            info_base_alias: "DemoBase".to_owned(),
        };
        let xml = local_variables_request(&session, "target-1", 0, "request-id");
        assert!(xml.contains(&format!(
            "<stackLevel xmlns=\"{DEBUG_CALCULATIONS_NAMESPACE}\">0</stackLevel>"
        )));
        assert!(xml.contains("<interfaces>context</interfaces>"));

        let variables = parse_local_variables_response(
            "<response><result><calculationResult><valueOfContextPropInfo><propInfo><propName>Counter</propName></propInfo><valueInfo><typeName>Number</typeName><pres>NDI=</pres></valueInfo></valueOfContextPropInfo></calculationResult></result></response>",
        )
        .unwrap();
        assert_eq!(
            variables,
            Some(vec![DebugVariable {
                name: "Counter".to_owned(),
                type_name: "Number".to_owned(),
                value: "42".to_owned(),
                ..DebugVariable::default()
            }])
        );
    }

    #[test]
    fn attaches_and_detaches_using_the_rdbg_http_endpoints() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_thread = thread::spawn(move || {
            let responses = [
                "",
                "<response><result>registered</result></response>",
                "<response></response>",
                "<response><id><id>target-1</id><seanceNo>1</seanceNo><userName>Demo</userName><targetType>Client</targetType></id></response>",
                "<response><result><cmdID>targetStarted</cmdID><targetID><id>target-1</id></targetID></result></response>",
                "<response></response>",
                "<response></response>",
                "<response></response>",
            ];
            let mut requests = Vec::new();

            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("Content-Length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                requests.push(format!(
                    "{request_line}{}",
                    String::from_utf8(body).unwrap()
                ));

                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
                stream.flush().unwrap();
            }
            requests
        });

        let server = DebugServer::new("127.0.0.1", port).unwrap();
        let session = server.attach_debug_ui("DemoBase").unwrap();
        server
            .set_auto_attach_types(&session, &["Client".to_owned(), "HttpService".to_owned()])
            .unwrap();
        assert_eq!(
            server.get_debug_targets(&session).unwrap(),
            vec![DebugTarget {
                id: "target-1".to_owned(),
                seance_no: "1".to_owned(),
                user_name: "Demo".to_owned(),
                target_type: "Client".to_owned(),
            }]
        );
        assert_eq!(
            match server.ping_debug_ui_timed(&session, None).unwrap() {
                PingOutcome::Events(result) => result.events,
                PingOutcome::TimedOut => panic!("unbounded ping unexpectedly timed out"),
            },
            vec![DebugUiEvent {
                command_id: "targetStarted".to_owned(),
                target_id: Some("target-1".to_owned()),
                ..DebugUiEvent::default()
            }]
        );
        server.clear_break_on_next_statement(&session).unwrap();
        server
            .attach_debug_targets(&session, &["target-1".to_owned()])
            .unwrap();
        server.detach_debug_ui(&session).unwrap();
        let requests = server_thread.join().unwrap();

        assert!(requests[0].starts_with("POST /e1crdbg/rdbgTest?cmd=test HTTP/1.1"));
        assert!(requests[1].starts_with("POST /e1crdbg/rdbg?cmd=attachDebugUI HTTP/1.1"));
        assert!(requests[1].contains("<infoBaseAlias>DemoBase</infoBaseAlias>"));
        assert!(requests[1].contains("<foregroundAbility>true</foregroundAbility>"));
        assert!(requests[2].starts_with("POST /e1crdbg/rdbg?cmd=setAutoAttachSettings HTTP/1.1"));
        assert!(requests[2].contains(&format!(
            "<targetType xmlns=\"{AUTO_ATTACH_NAMESPACE}\">Client</targetType>"
        )));
        assert!(requests[2].contains(&format!(
            "<targetType xmlns=\"{AUTO_ATTACH_NAMESPACE}\">HTTPService</targetType>"
        )));
        assert!(requests[3].starts_with("POST /e1crdbg/rdbg?cmd=getDbgTargets HTTP/1.1"));
        assert!(requests[3].contains("<infoBaseAlias>DemoBase</infoBaseAlias>"));
        assert!(requests[4].starts_with("POST /e1crdbg/rdbg?cmd=pingDebugUIParams&dbgui="));
        assert!(
            requests[5].starts_with("POST /e1crdbg/rdbg?cmd=clearBreakOnNextStatement HTTP/1.1")
        );
        assert!(requests[5].contains(&format!(
            "<idOfDebuggerUI>{}</idOfDebuggerUI>",
            session.id()
        )));
        assert!(requests[6].starts_with("POST /e1crdbg/rdbg?cmd=attachDetachDbgTargets HTTP/1.1"));
        assert!(requests[6].contains(&format!(
            "<attach>true</attach><id><id xmlns=\"{DEBUG_BASE_NAMESPACE}\">target-1</id></id>"
        )));
        assert!(requests[7].starts_with("POST /e1crdbg/rdbg?cmd=detachDebugUI HTTP/1.1"));
        assert!(requests[7].contains(&format!(
            "<idOfDebuggerUI>{}</idOfDebuggerUI>",
            session.id()
        )));
    }
}
