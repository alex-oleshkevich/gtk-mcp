//! Parameters of the `layout-style` toolset (`docs/tools.md`, milestone M3).
//! Same conventions as `tools.rs`: every params type carries `session`.

use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_session() -> String {
    String::new()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectLayoutParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    /// Also measure the widget at this size (pixels, >= 0): as a width (height
    /// requested for it) and as a height (width requested for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_size: Option<i32>,
}

/// Audit rules selectable through `AuditLayoutParams::checks`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutCheck {
    EllipsizedLabel,
    OutsideParent,
    ZeroSize,
    OverlappingSiblings,
    UnresolvedIcon,
    InscriptionEllipsis,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuditLayoutParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Subtree to audit (ref or selector); omitted: all toplevels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// Rules to run; omitted: all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<LayoutCheck>>,
    /// Maximum findings returned (default 100); `total` is always reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectStyleParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplyCssParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// CSS text, installed on the display at user priority.
    pub css: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoveCssParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Handle returned by `apply_css` or `highlight`.
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HighlightParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// Widget to outline; omitted: only clear this client's previous highlight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectRenderParams {
    #[serde(default = "default_session")]
    pub session: String,
    pub target: Target,
    /// Maximum nodes returned (default 500); `truncated` reports the cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
    /// Maximum depth (default 32).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectSettingsParams {
    #[serde(default = "default_session")]
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OverrideSettingParams {
    #[serde(default = "default_session")]
    pub session: String,
    /// `color_scheme` (AdwColorScheme nick: default, force-light, prefer-light,
    /// prefer-dark, force-dark; without libadwaita: light, dark, default),
    /// `animations` (bool), `text_scale` (number, factor of the original text
    /// scale), `text_direction` (`ltr` | `rtl`). `high_contrast` and
    /// `accent_color` are read-only (`unsupported_operation`).
    pub setting: String,
    /// New value; not needed with `reset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Restore the value the setting had before this session's override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}
