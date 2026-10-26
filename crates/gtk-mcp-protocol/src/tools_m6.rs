//! Parameter types of the `a11y` toolset (`docs/tools.md` a11y, M6).
//!
//! `accessible_tree` and `inspect_accessible` are answered by the agent from
//! the `GtkAccessible` hierarchy; the broker then joins the AT-SPI export of
//! the same process onto it (structural walk, spike S3, `docs/plan.md` 3.2).
//! `audit_accessibility` runs its rules in the broker over that joined tree.

use crate::types::Target;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccessibleTreeParams {
    #[serde(default)]
    pub session: String,
    /// A widget ref or selector; omitted: all toplevels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Target>,
    /// Default 64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Default 2000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectAccessibleParams {
    #[serde(default)]
    pub session: String,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuditAccessibilityParams {
    #[serde(default)]
    pub session: String,
    /// Restrict the audit to this subtree; omitted: all toplevels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Target>,
    /// Only these rules (names from the result's `rules`); omitted: all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
    /// Maximum nodes examined (default 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TraceFocusOrderParams {
    #[serde(default)]
    pub session: String,
    /// Window (or widget inside it) whose Tab chain is walked; omitted: the active toplevel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Target>,
    /// Maximum Tab steps (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
}
