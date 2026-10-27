//! M4 tool params: lifecycle (`launch_app`, `restart_app`, `stop_app`), the
//! `app` toolset, `object_stats`, and the app-state wire methods
//! (`docs/tools.md` core / app / diagnostics, `docs/plan.md` 4.5, 5.5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Agent methods behind the `app-state/<name>` MCP resources. They are not
/// tools: the broker calls them to list and read `register_state` closures.
pub mod state_method {
    pub const LIST: &str = "state/list";
    pub const READ: &str = "state/read";
}

/// Prefix of every app-state resource name (`app-state/<name>`).
pub const STATE_PREFIX: &str = "app-state/";

/// Default bound for `launch_app` / `restart_app` to wait for the session socket.
pub const DEFAULT_LAUNCH_TIMEOUT_MS: u64 = 15_000;
/// Default delay before `stop_app` escalates from SIGTERM to SIGKILL.
pub const DEFAULT_STOP_TIMEOUT_MS: u64 = 3_000;

fn default_session() -> String {
    String::new()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum A11yPreset {
    /// `GTK_A11Y=none`
    None,
    /// `GTK_A11Y=atspi`
    Atspi,
    /// `GTK_A11Y=test`
    Test,
}

impl A11yPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Atspi => "atspi",
            Self::Test => "test",
        }
    }
}

/// GTK presets of `launch_app`. Each maps to one environment variable of the
/// child (inherited by the app's own children, `docs/plan.md` 4.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LaunchPresets {
    /// `GDK_DEBUG=no-portals`: in-process file chooser, no portal OpenURI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_portals: Option<bool>,
    /// `GOBJECT_DEBUG=instance-count`: required by `object_stats`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_count: Option<bool>,
    /// `GTK_A11Y`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a11y: Option<A11yPreset>,
    /// `LANGUAGE`, e.g. `de` or `de:en`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// `GSK_RENDERER`, e.g. `ngl`, `vulkan`, `cairo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gsk_renderer: Option<String>,
    /// `GTK_DEBUG`, e.g. `interactive,layout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gtk_debug: Option<String>,
}

/// Broker-launched app. `session` is unused (a launch has none yet).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LaunchAppParams {
    #[serde(default = "default_session", skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// Program and arguments; executed directly, never through a shell.
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Extra environment, passed through like any other variable. The broker
    /// itself sets `GTK_MCP_DIR` to its socket directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<LaunchPresets>,
    /// `WAYLAND_DISPLAY` for the app; default: the broker's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wayland_display: Option<String>,
    /// `XDG_RUNTIME_DIR` for the app (where its Wayland socket lives); default: the broker's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xdg_runtime_dir: Option<String>,
    /// Bound for the session socket to appear (default 15000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct StopAppParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// SIGKILL the process group this long after SIGTERM (default 3000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RestartAppParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// SIGKILL delay for the old process (default 3000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Bound for the new session socket to appear (default 15000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AppOpenParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Files to open (absolute paths or URIs): `g_application_open`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Hint passed to the `open` signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// `g_application_activate`; the default when `files` is empty. Not combinable with `files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InspectGsettingsParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// One schema id (any installed schema); default: the app's schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Path of a relocatable schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SetGsettingParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub schema: String,
    /// Path of a relocatable schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub key: String,
    /// JSON value converted by the key's GVariant type (booleans, numbers,
    /// strings, arrays, tuples, string-keyed dictionaries). Exactly one of
    /// `value` and `value_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// GVariant text format, for types JSON cannot express.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ListResourcesParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Resource path to enumerate (default `/`); directories end in `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Descend into directories (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
    /// Maximum entries returned (default 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEncoding {
    /// UTF-8 text when the (possibly truncated) bytes are valid, else base64.
    #[default]
    Auto,
    Text,
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ReadResourceParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub path: String,
    /// Bytes returned at most (default 65536, maximum 1048576).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<ResourceEncoding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ObjectStatsParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Only types whose name starts with this prefix (e.g. `Gtk`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_prefix: Option<String>,
    /// Only types with at least this many live instances (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_count: Option<u32>,
    /// Maximum types returned, highest counts first (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

pub const MAX_RESOURCE_READ: u64 = 1024 * 1024;
pub const DEFAULT_RESOURCE_READ: u64 = 64 * 1024;
