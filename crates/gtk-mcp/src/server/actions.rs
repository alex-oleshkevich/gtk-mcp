//! rmcp wrappers of the `actions` toolset (M2). Same pattern as the core
//! tools in the parent module; kept in its own router so the toolset stays a
//! single additive block.

use super::Server;
use gtk_mcp_protocol::*;
use rmcp::{
    ErrorData, RoleServer, handler::server::wrapper::Parameters, model::CallToolResult, service::RequestContext, tool,
    tool_router,
};

#[tool_router(router = tool_router_actions, vis = "pub(super)")]
impl Server {
    #[tool(description = "Actions of the app (`app.`), the active window (`win.`, registered groups, widget-class actions) or a widget (everything reachable from it), each with source, enabled, parameter type, state type/value/hint. `complete` is false where groups inserted with insert_action_group may exist unregistered (they still activate by name). Widget-class actions report enabled null.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn list_actions(&self, Parameters(p): Parameters<ListActionsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_actions", p, ctx).await
    }

    #[tool(description = "One action addressed as {scope, name}: parameter type, state type/value, enabled (null for widget-class actions), state hint.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_action(&self, Parameters(p): Parameters<InspectActionParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_action", p, ctx).await
    }

    #[tool(description = "Activate an action with a typed parameter (JSON converted by the action's parameter type; invalid -> invalid_value). Returns the observed state afterwards. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn activate_action(&self, Parameters(p): Parameters<ActivateActionParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("activate_action", p, ctx).await
    }

    #[tool(description = "Request a state change of a stateful action; returns the observed state and accepted=false when the app rejected or altered the request. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn change_action_state(&self, Parameters(p): Parameters<ChangeActionStateParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("change_action_state", p, ctx).await
    }

    #[tool(description = "Action name and target of an actionable widget (GtkActionable, menu buttons, widgets with action-name), with the resolved action's enabled/state.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_action_binding(&self, Parameters(p): Parameters<InspectActionBindingParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_action_binding", p, ctx).await
    }

    #[tool(description = "Walk a GMenuModel (menu button / popover menu model, application menubar): sections, submenus, items with label, action, typed target, attributes and links. Bounded by max_depth and max_items.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_menu(&self, Parameters(p): Parameters<InspectMenuParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_menu", p, ctx).await
    }

    #[tool(description = "Shortcut controllers (local, managed, global) with trigger strings and actions, plus the application's accelerators.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_shortcuts(&self, Parameters(p): Parameters<InspectShortcutsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_shortcuts", p, ctx).await
    }

    #[tool(description = "Match a keyval trigger (e.g. <Control>s) against shortcut controllers from the focus widget upward plus managed/global ones and run its GtkShortcutAction. Comparison only, not a key press; not_found when nothing matches. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn activate_shortcut(&self, Parameters(p): Parameters<ActivateShortcutParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("activate_shortcut", p, ctx).await
    }

    #[tool(description = "Event controllers and gestures of a widget: type, propagation phase and limit, name, gesture configuration (button, n_points, touch_only, exclusive).", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_controllers(&self, Parameters(p): Parameters<InspectControllersParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_controllers", p, ctx).await
    }
}
