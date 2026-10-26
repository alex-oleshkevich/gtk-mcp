//! rmcp wrappers of the `a11y` toolset (M6).

use super::Server;
use gtk_mcp_protocol::*;
use rmcp::{
    ErrorData, RoleServer, handler::server::wrapper::Parameters, model::CallToolResult, service::RequestContext, tool,
    tool_router,
};

#[tool_router(router = tool_router_a11y, vis = "pub(super)")]
impl Server {
    #[tool(description = "Accessible hierarchy (GtkAccessible) of all toplevels or a subtree, each node with ref, role, bounds and platform state. With --allow-atspi the app's AT-SPI tree is joined structurally: joined nodes carry an `atspi` object (name, description, states, interfaces; source gtk+atspi), AT-SPI-only nodes have ref null. Without it `atspi.available` is false.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn accessible_tree(&self, Parameters(p): Parameters<AccessibleTreeParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("accessible_tree", p, ctx).await
    }

    #[tool(description = "One widget's accessibility data, each field tagged with its source: GTK (role, parent, children, bounds, platform state) and, with --allow-atspi, AT-SPI (name, description, states, relations, actions, text, value, attributes). Without AT-SPI those fields are reported unavailable; secret widgets' name/text/value are redacted.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_accessible(&self, Parameters(p): Parameters<InspectAccessibleParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_accessible", p, ctx).await
    }

    #[tool(description = "Accessibility findings with ref and evidence: interactive widgets not focusable, icon-only buttons without label, small targets, untitled windows (GTK rules); with --allow-atspi also empty accessible names, unnamed images, unexported widgets. AT-SPI rules are reported skipped (with reason) when the source is off.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn audit_accessibility(&self, Parameters(p): Parameters<AuditAccessibilityParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("audit_accessibility", p, ctx).await
    }

    #[tool(description = "Walk the Tab chain of a window with gtk_widget_child_focus and report order, loops, focus traps and focusable widgets never reached; the previous focus is restored. Side effects possible: scroll-to-focus, popover autohide, select-on-focus. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn trace_focus_order(&self, Parameters(p): Parameters<TraceFocusOrderParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("trace_focus_order", p, ctx).await
    }
}
