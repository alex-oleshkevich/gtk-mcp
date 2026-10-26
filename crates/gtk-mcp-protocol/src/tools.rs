//! Tool parameter and result types shared by broker and agent
//! (`docs/plan.md` section 8: defined once, forwarded as JSON-RPC params).
//!
//! Every params type carries `session`; the broker resolves it, the agent
//! ignores it. Inside a `batch` step the agent injects the batch's session
//! before parsing, so step arguments never repeat it.

use crate::tools_m2::*;
use crate::types::*;
use crate::tools_m3::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_session() -> String {
    String::new()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotView {
    #[default]
    Full,
    Semantic,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct EmptyParams {
    #[serde(default = "default_session")]
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListWindowsParams {
    #[serde(default = "default_session")]
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// A ref or selector; omitted: all toplevels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<SnapshotView>,
    /// `text` (default) or `json`; handled by the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<SnapshotFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_unmapped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_closed_popovers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub selector: Selector,
    /// Maximum matches returned (default 100); `total` is always reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListPropertiesParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    /// Only properties declared by this class (GType name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Only the property with this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetPropertyParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetPropertyParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    pub name: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListSignalsParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectTextParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    /// Character offsets; default the whole buffer (bounded by `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_tags: Option<bool>,
    /// Maximum characters returned (default 4096).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum EditTextOp {
    Insert { offset: i32, text: String },
    Delete { start: i32, end: i32 },
    Replace { start: i32, end: i32, text: String },
    SetSelection { start: i32, end: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditTextParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    #[serde(flatten)]
    pub op: EditTextOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

/// What `wait_for` waits on. Evaluated against the selector/ref in `target`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "condition", rename_all = "snake_case")]
pub enum WaitCondition {
    /// At least one live match exists.
    Exists,
    /// No live match exists (or the ref went stale).
    Absent,
    /// Exactly `count` matches.
    MatchCount { count: usize },
    /// Property `name` equals `equals` (after the agent's value conversion).
    Property { name: String, equals: Value },
    /// Text of the target equals / contains. Never satisfied by secret objects.
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equals: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
    },
    /// Target mapped state equals `mapped` (default true).
    Mapped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mapped: Option<bool>,
    },
    /// Target sensitive state equals `sensitive` (default true).
    Sensitive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
    },
    /// `GApplication:is-busy` equals `busy` (default false). `target` is ignored.
    AppBusy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        busy: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitForParams {
    #[serde(default = "default_session")]
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(flatten)]
    pub condition: WaitCondition,
    /// Default 5000, maximum 60000. A timeout returns `satisfied: false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectModelParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffSnapshotsParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffCapturesParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub from: String,
    pub to: String,
}

/// Agent reply to `capture`: the contract `CaptureResult` plus the PNG.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CaptureResponse {
    #[serde(flatten)]
    pub result: CaptureResult,
    pub png_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatchStepResult {
    pub tool: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatchResult {
    pub steps: Vec<BatchStepResult>,
    /// Index of the first failing step when the batch stopped early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<usize>,
    /// The broker cancelled the request mid-batch; `steps` holds what ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
}

// -- tool registry -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub toolset: &'static str,
    pub mutating: bool,
    /// Runs in the agent (forwarded) vs. answered by the broker alone.
    pub agent: bool,
}

pub const TOOLSETS: &[&str] = &["core", "actions", "layout-style", "a11y", "diagnostics", "app"];

/// Tools registered by this build (milestones M0 and M1).
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec { name: "list_apps", toolset: "core", mutating: false, agent: false },
    ToolSpec { name: "list_windows", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "snapshot", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "diff_snapshots", toolset: "core", mutating: false, agent: false },
    ToolSpec { name: "query", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "inspect", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "list_properties", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "get_property", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "set_property", toolset: "core", mutating: true, agent: true },
    ToolSpec { name: "list_signals", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "interact", toolset: "core", mutating: true, agent: true },
    ToolSpec { name: "inspect_text", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "edit_text", toolset: "core", mutating: true, agent: true },
    ToolSpec { name: "wait_for", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "batch", toolset: "core", mutating: true, agent: true },
    ToolSpec { name: "inspect_model", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "capture", toolset: "core", mutating: false, agent: true },
    ToolSpec { name: "diff_captures", toolset: "core", mutating: false, agent: false },
    // layout-style (M3)
    ToolSpec { name: "inspect_layout", toolset: "layout-style", mutating: false, agent: true },
    ToolSpec { name: "audit_layout", toolset: "layout-style", mutating: false, agent: true },
    ToolSpec { name: "inspect_style", toolset: "layout-style", mutating: false, agent: true },
    ToolSpec { name: "apply_css", toolset: "layout-style", mutating: true, agent: true },
    ToolSpec { name: "remove_css", toolset: "layout-style", mutating: true, agent: true },
    ToolSpec { name: "highlight", toolset: "layout-style", mutating: true, agent: true },
    ToolSpec { name: "inspect_render", toolset: "layout-style", mutating: false, agent: true },
    ToolSpec { name: "inspect_settings", toolset: "layout-style", mutating: false, agent: true },
    ToolSpec { name: "override_setting", toolset: "layout-style", mutating: true, agent: true },
    ToolSpec { name: "list_actions", toolset: "actions", mutating: false, agent: true },
    ToolSpec { name: "inspect_action", toolset: "actions", mutating: false, agent: true },
    ToolSpec { name: "activate_action", toolset: "actions", mutating: true, agent: true },
    ToolSpec { name: "change_action_state", toolset: "actions", mutating: true, agent: true },
    ToolSpec { name: "inspect_action_binding", toolset: "actions", mutating: false, agent: true },
    ToolSpec { name: "inspect_menu", toolset: "actions", mutating: false, agent: true },
    ToolSpec { name: "inspect_shortcuts", toolset: "actions", mutating: false, agent: true },
    ToolSpec { name: "activate_shortcut", toolset: "actions", mutating: true, agent: true },
    ToolSpec { name: "inspect_controllers", toolset: "actions", mutating: false, agent: true },
];

pub fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Validate a batch step's arguments against the tool's typed params.
/// Returns the tool's spec. `batch` itself and broker-only tools are not
/// batchable (no nesting, no lifecycle; diffs need broker-side state).
pub fn validate_step(tool: &str, arguments: &Value) -> Result<&'static ToolSpec, String> {
    let spec = tool_spec(tool).ok_or_else(|| format!("unknown tool '{tool}'"))?;
    if tool == "batch" {
        return Err("batch steps cannot be nested".into());
    }
    if !spec.agent {
        return Err(format!("tool '{tool}' cannot be used inside a batch"));
    }
    let mut args = arguments.clone();
    if args.is_null() {
        args = Value::Object(Default::default());
    }
    if !args.is_object() {
        return Err(format!("arguments of step '{tool}' must be an object"));
    }
    args.as_object_mut().unwrap().entry("session").or_insert(Value::String(String::new()));
    parse_params(tool, args).map_err(|e| format!("invalid arguments for step '{tool}': {e}"))?;
    Ok(spec)
}

/// Deserialize `args` into the params type of `tool` (shape check only).
pub fn parse_params(tool: &str, args: Value) -> Result<(), String> {
    macro_rules! chk {
        ($t:ty) => {
            serde_json::from_value::<$t>(args).map(|_| ()).map_err(|e| e.to_string())
        };
    }
    match tool {
        "list_apps" => chk!(EmptyParams),
        "list_windows" => chk!(ListWindowsParams),
        "snapshot" => chk!(SnapshotParams),
        "diff_snapshots" => chk!(DiffSnapshotsParams),
        "query" => chk!(QueryParams),
        "inspect" => chk!(InspectParams),
        "list_properties" => chk!(ListPropertiesParams),
        "get_property" => chk!(GetPropertyParams),
        "set_property" => chk!(SetPropertyParams),
        "list_signals" => chk!(ListSignalsParams),
        "interact" => chk!(InteractParams),
        "inspect_text" => chk!(InspectTextParams),
        "edit_text" => chk!(EditTextParams),
        "wait_for" => chk!(WaitForParams),
        "batch" => chk!(BatchParams),
        "inspect_model" => chk!(InspectModelParams),
        "capture" => chk!(CaptureParams),
        "diff_captures" => chk!(DiffCapturesParams),
        "inspect_layout" => chk!(InspectLayoutParams),
        "audit_layout" => chk!(AuditLayoutParams),
        "inspect_style" => chk!(InspectStyleParams),
        "apply_css" => chk!(ApplyCssParams),
        "remove_css" => chk!(RemoveCssParams),
        "highlight" => chk!(HighlightParams),
        "inspect_render" => chk!(InspectRenderParams),
        "inspect_settings" => chk!(InspectSettingsParams),
        "override_setting" => chk!(OverrideSettingParams),
        "list_actions" => chk!(ListActionsParams),
        "inspect_action" => chk!(InspectActionParams),
        "activate_action" => chk!(ActivateActionParams),
        "change_action_state" => chk!(ChangeActionStateParams),
        "inspect_action_binding" => chk!(InspectActionBindingParams),
        "inspect_menu" => chk!(InspectMenuParams),
        "inspect_shortcuts" => chk!(InspectShortcutsParams),
        "activate_shortcut" => chk!(ActivateShortcutParams),
        "inspect_controllers" => chk!(InspectControllersParams),
        other => Err(format!("unknown tool '{other}'")),
    }
}
