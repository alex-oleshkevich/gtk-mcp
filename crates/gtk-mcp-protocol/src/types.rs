//! Contract types, `docs/plan.md` section 7. Names and fields are normative.
//!
//! `skip_serializing_if` / `deny_unknown_fields` attributes only affect the
//! wire shape of absent optionals and typo detection; they never rename or
//! drop a field.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024; // PNGs travel base64 inside JSON

/// Method names. Broker → agent: `hello`, tool names, `$/cancel`.
/// Agent → broker notifications: `$/event`, `$/log`, `$/progress`, `$/late_result`.
pub mod method {
    pub const HELLO: &str = "hello";
    pub const CANCEL: &str = "$/cancel";
    pub const EVENT: &str = "$/event";
    pub const LOG: &str = "$/log";
    pub const PROGRESS: &str = "$/progress";
    pub const LATE_RESULT: &str = "$/late_result";
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub broker_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct HelloAck {
    pub protocol: u32,
    pub agent_version: String,
    pub pid: u32,
    pub app_id: Option<String>,
    pub gtk_version: [u32; 3],
    pub adw_version: Option<[u32; 3]>,
    pub capabilities: Vec<Capability>,
    pub mutations_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Adwaita,
    LayerShell,
    AccessibleId,
    LogForwarding,
    InstanceCount,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RequestMeta {
    pub deadline_ms: u64,
} // carried in params._meta

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Cancel {
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LateResult {
    pub id: u64,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidParams,
    NotFound,
    AmbiguousTarget,
    StaleReference,
    Detached,
    TargetUnmapped,
    UnsupportedOperation,
    InvalidValue,
    MutationsDisabled,
    LaunchDisabled,
    Redacted,
    Timeout,
    NotStarted,
    TargetUnresponsive,
    CaptureUnavailable,
    SnapshotUnavailable,
    SessionExited,
    VersionMismatch,
    AgentInactive,
    Unavailable, // feature needs a preset/diagnostic that is off (e.g. instance counts)
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidParams => "invalid_params",
            Self::NotFound => "not_found",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::StaleReference => "stale_reference",
            Self::Detached => "detached",
            Self::TargetUnmapped => "target_unmapped",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::InvalidValue => "invalid_value",
            Self::MutationsDisabled => "mutations_disabled",
            Self::LaunchDisabled => "launch_disabled",
            Self::Redacted => "redacted",
            Self::Timeout => "timeout",
            Self::NotStarted => "not_started",
            Self::TargetUnresponsive => "target_unresponsive",
            Self::CaptureUnavailable => "capture_unavailable",
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::SessionExited => "session_exited",
            Self::VersionMismatch => "version_mismatch",
            Self::AgentInactive => "agent_inactive",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for AgentError {}

// Targets: a ref string ("w12", "o4") or a selector object.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum Target {
    Ref(String),
    Select(Selector),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buildable_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>, // matches the GType or any ancestor type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>, // exact; never matches secret objects
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_contains: Option<String>, // never matches secret objects
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ancestor: Option<Box<Selector>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_detached: Option<bool>, // default false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<usize>, // disambiguate in tree order
}
// All present keys must match (AND). Single-target tools require exactly one
// match after `nth`; otherwise AmbiguousTarget with up to 10 candidates.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowSel {
    Ref(String),
    Title(String),
    Monitor(String),        // GdkMonitor connector, e.g. "DP-1" (one glimpse panel per output)
    AppWindowId(u32),       // gtk_application_window_get_id
    LayerNamespace(String), // when layer shell is loaded
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StateFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_focus: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum InteractOp {
    Activate,
    Focus,
    Fill { text: String },
    SetChecked { checked: bool },
    SetActive { active: bool },
    SetValue { value: f64 },
    SetExpanded { expanded: bool },
    Select { index: u32 },
    SelectPage { name: String },
    Popup,
    Popdown,
    ScrollTo,
    ScrollBy { dx: f64, dy: f64 },
    Close,
    Play,
    Pause,
    Seek { microseconds: i64 },
}

impl InteractOp {
    /// The operation's wire name, as listed in node `capabilities`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Focus => "focus",
            Self::Fill { .. } => "fill",
            Self::SetChecked { .. } => "set_checked",
            Self::SetActive { .. } => "set_active",
            Self::SetValue { .. } => "set_value",
            Self::SetExpanded { .. } => "set_expanded",
            Self::Select { .. } => "select",
            Self::SelectPage { .. } => "select_page",
            Self::Popup => "popup",
            Self::Popdown => "popdown",
            Self::ScrollTo => "scroll_to",
            Self::ScrollBy { .. } => "scroll_by",
            Self::Close => "close",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Seek { .. } => "seek",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
pub struct Observe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_depth: Option<u32>, // snapshot of the target subtree after the action
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<bool>, // settled capture of the target after the action
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InteractParams {
    #[serde(default)]
    pub session: String,
    pub target: Target,
    #[serde(flatten)]
    pub op: InteractOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InteractResult {
    pub target: String,              // resolved ref
    pub observed: serde_json::Value, // state read back after the operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_error: Option<AgentError>, // dispatch succeeded, observation failed
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BatchStep {
    pub tool: String,
    pub arguments: serde_json::Value,
} // "$N.ref" allowed in targets

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BatchParams {
    #[serde(default)]
    pub session: String,
    pub steps: Vec<BatchStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CaptureParams {
    #[serde(default)]
    pub session: String,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<bool>, // default true
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>, // default 2000
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotate: Option<bool>, // overlay refs of visible descendants
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CaptureResult {
    pub capture_id: String,
    pub target: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Annotation {
    pub r#ref: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Incompatible,
    Exited { code: Option<i32>, signal: Option<i32>, in_flight: Option<u64> },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SessionInfo {
    pub session: String,
    pub pid: u32,
    pub app_id: Option<String>,
    pub launched_by_broker: bool,
    pub state: SessionState,
    pub hello: Option<HelloAck>,
}
