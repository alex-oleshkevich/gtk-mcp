//! M5 tool params: the `diagnostics` toolset (`watch`, `read_events`,
//! `unwatch`, `read_logs`, `measure_frames`, `record`, `stop_recording`;
//! `docs/tools.md` diagnostics, `docs/plan.md` 5.6 and 8).

use crate::types::Target;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_session() -> String {
    String::new()
}

/// Default and maximum number of entries `read_events` / `read_logs` return per call.
pub const DEFAULT_READ_LIMIT: u32 = 200;
pub const MAX_READ_LIMIT: u32 = 2000;
/// Default and maximum `measure_frames` window.
pub const DEFAULT_FRAME_WINDOW_MS: u64 = 1000;
pub const MAX_FRAME_WINDOW_MS: u64 = 10_000;
/// Emissions within this many milliseconds after a real input event are `human` (plan 5.6, spike S4).
pub const HUMAN_WINDOW_MS: u64 = 300;

/// What a subscription observes on its target.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchKind {
    /// `properties` omitted: every property.
    Notify {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        properties: Option<Vec<String>>,
    },
    /// See `list_signals`, `watch.eligible`.
    Signal { signal: String },
    /// Target is a list or selection model, or a widget with a `model` property.
    Model,
    /// App-wide, broker-side; needs `--allow-atspi`.
    Atspi {
        /// Event-name prefixes; omitted or empty: focus, state, name and window events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        events: Option<Vec<String>>,
    },
}

impl WatchKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Notify { .. } => "notify",
            Self::Signal { .. } => "signal",
            Self::Model => "model",
            Self::Atspi { .. } => "atspi",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct WatchParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Required for notify, signal and model; omitted for app-wide AT-SPI events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(flatten)]
    pub watch: WatchKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct UnwatchParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub subscription: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ReadEventsParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Cursor to read from (the previous `next_cursor`); default: the oldest retained event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    /// Maximum events returned (default 200, maximum 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only events of this subscription (`late_result` records are kept when `kind` is `late_result`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    /// Only events of this kind: `notify`, `signal`, `items_changed`, `selection_changed`, `atspi`, `late_result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LogSeverity {
    Debug,
    Info,
    Message,
    Warning,
    Critical,
    Error,
}

impl LogSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Message => "message",
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Error => "error",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "debug" => Self::Debug,
            "info" => Self::Info,
            "message" => Self::Message,
            "warning" | "warn" => Self::Warning,
            "critical" => Self::Critical,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ReadLogsParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Cursor to read from; default: the oldest retained record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    /// Maximum records returned (default 200, maximum 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only records at or above this severity; unparsed lines (no severity) are kept only when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_level: Option<LogSeverity>,
    /// Only records whose text/message contains this string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Only records from this source: `stdout`, `stderr` or `forwarded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MeasureFramesParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Window or widget whose native's frame clock is measured; default: the active (else first) toplevel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// Measurement window (default 1000 ms, maximum 10000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RecordParams {
    #[serde(default = "default_session")]
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct StopRecordingParams {
    #[serde(default = "default_session")]
    pub session: String,
}
