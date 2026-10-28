//! Parameter types of the `actions` toolset (`docs/tools.md` actions, M2).
//!
//! Actions are not objects; they are addressed as `{scope, name}`:
//!
//! * `scope` is a [`Target`]. The reserved refs `"app"` (the `GtkApplication`)
//!   and `"window"` (the active `GtkApplicationWindow`) select those scopes;
//!   any other ref or selector names a widget. When `scope` is omitted it is
//!   inferred from the prefix of `name` (`app.` or `win.`).
//! * `name` carries the action prefix (`win.save`, `panel.refresh`). With
//!   scope `app` or `window` the prefix may be omitted.

use crate::types::{Observe, Target};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListActionsParams {
    #[serde(default)]
    pub session: String,
    /// `"app"`, `"window"`, or a widget ref/selector. Default: `"app"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Target>,
    /// Only actions with this prefix (`win`, `app`, `panel`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectActionParams {
    #[serde(default)]
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Target>,
    /// Action name, e.g. `win.save`.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivateActionParams {
    #[serde(default)]
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Target>,
    pub name: String,
    /// Typed by the action's parameter type: booleans, integers, numbers,
    /// strings, arrays (arrays and tuples), objects (`a{s*}`), `null`
    /// (`maybe`), `{"type","value"}` (`v`); any non-string type also accepts
    /// GVariant text. Omit for actions without a parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChangeActionStateParams {
    #[serde(default)]
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Target>,
    pub name: String,
    /// The requested state, converted by the action's state type like
    /// `activate_action` parameters.
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectActionBindingParams {
    #[serde(default)]
    pub session: String,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectMenuParams {
    #[serde(default)]
    pub session: String,
    /// A widget with a `menu-model` (menu button, popover menu, menu bar), an
    /// `o<N>` ref of a `GMenuModel`, or `"app"` for the application menubar.
    pub target: Target,
    /// Section/submenu nesting below the root menu that is expanded (default
    /// 8; 0 lists only the root's items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum items returned in total (default 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectShortcutsParams {
    #[serde(default)]
    pub session: String,
    /// A widget: its shortcut controllers and those of its descendants.
    /// Omitted: every window of the app, plus the application's accelerators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// Maximum controller and application-accelerator entries returned in
    /// total (default 200); `total` includes omitted entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivateShortcutParams {
    #[serde(default)]
    pub session: String,
    /// A keyval trigger in accelerator syntax, e.g. `<Control>s`.
    pub trigger: String,
    /// Widget the search starts from (default: the focus widget of the active
    /// window, else the window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<Observe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectControllersParams {
    #[serde(default)]
    pub session: String,
    pub target: Target,
}
