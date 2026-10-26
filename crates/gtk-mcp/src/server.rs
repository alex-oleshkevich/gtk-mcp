//! rmcp front end: one thin `#[tool]` method per tool, all delegating to
//! [`Broker::call_tool_with_progress`]. Tools filtered out by policy are removed
//! from the router, so they are absent from `tools/list` and from dispatch.

use crate::broker::{Broker, Content};
use crate::session::{Progress, ProgressSink};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gtk_mcp_protocol::*;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ProgressNotificationParam, ServerCapabilities, ServerConfig},
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct Server {
    broker: Arc<Broker>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub fn new(broker: Arc<Broker>) -> Result<Self, String> {
        let mut router = Self::tool_router();
        let registered: Vec<String> = router.list_all().iter().map(|t| t.name.to_string()).collect();
        for t in TOOLS {
            if !registered.iter().any(|n| n == t.name) {
                return Err(format!("tool '{}' is registered in the protocol but has no MCP handler", t.name));
            }
        }
        for name in registered {
            if !broker.is_enabled(&name) {
                router.remove_route(&name);
            }
        }
        Ok(Self { broker, tool_router: router })
    }

    async fn run<P: Serialize>(
        &self,
        name: &str,
        params: P,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = serde_json::to_value(params).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let sink = ctx.meta.get_progress_token().map(|token| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
            let peer = ctx.peer.clone();
            tokio::spawn(async move {
                while let Some(p) = rx.recv().await {
                    let mut n = ProgressNotificationParam::new(token.clone(), p.progress);
                    if let Some(t) = p.total {
                        n = n.with_total(t);
                    }
                    if let Some(m) = p.message {
                        n = n.with_message(m);
                    }
                    let _ = peer.notify_progress(n).await;
                }
            });
            Arc::new(move |p: Progress| {
                let _ = tx.send(p);
            }) as ProgressSink
        });
        Ok(match self.broker.call_tool_with_progress(name, args, sink).await {
            Ok(out) => CallToolResult::success(
                out.contents
                    .into_iter()
                    .map(|c| match c {
                        Content::Text(t) => ContentBlock::text(t),
                        Content::Png(p) => ContentBlock::image(STANDARD.encode(p), "image/png"),
                    })
                    .collect(),
            ),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(serde_json::to_string_pretty(&e).unwrap_or_default())]),
        })
    }
}

#[tool_router]
impl Server {
    #[tool(description = "List instrumented GTK apps (sessions): pid, app id, GTK/libadwaita versions, capabilities, mutations_enabled, state (running, incompatible, exited). Rescans the socket directory.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn list_apps(&self, Parameters(p): Parameters<EmptyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_apps", p, ctx).await
    }

    #[tool(description = "Toplevel windows and mapped native surfaces: title, visibility, size, refs, monitor, renderer.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn list_windows(&self, Parameters(p): Parameters<ListWindowsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_windows", p, ctx).await
    }

    #[tool(description = "Widget tree (view full|semantic, format text|json). Returns a snapshot_id retained by the broker for diff_snapshots.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("snapshot", p, ctx).await
    }

    #[tool(description = "Compare two retained snapshots by snapshot_id: added/removed widgets and changed state, text, label, attached, child count and size per widget ref. Evicted ids return snapshot_unavailable.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn diff_snapshots(&self, Parameters(p): Parameters<DiffSnapshotsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("diff_snapshots", p, ctx).await
    }

    #[tool(description = "Resolve a selector to all matching widget refs.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn query(&self, Parameters(p): Parameters<QueryParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("query", p, ctx).await
    }

    #[tool(description = "Type, ancestry, identities, state (live/detached), capabilities and related objects of a target.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect(&self, Parameters(p): Parameters<InspectParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect", p, ctx).await
    }

    #[tool(description = "GObject property schemas of a target, filterable by owner class or name.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn list_properties(&self, Parameters(p): Parameters<ListPropertiesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_properties", p, ctx).await
    }

    #[tool(description = "Read property values; object values as refs; secret objects are redacted.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn get_property(&self, Parameters(p): Parameters<GetPropertyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("get_property", p, ctx).await
    }

    #[tool(description = "Write a property and return the observed value. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn set_property(&self, Parameters(p): Parameters<SetPropertyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("set_property", p, ctx).await
    }

    #[tool(description = "Signal schemas of a target and watch eligibility.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn list_signals(&self, Parameters(p): Parameters<ListSignalsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_signals", p, ctx).await
    }

    #[tool(description = "Typed semantic operation (activate, focus, fill, set_checked, set_active, set_value, set_expanded, select, select_page, popup, popdown, scroll_to, scroll_by, close, play, pause, seek) with state readback. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn interact(&self, Parameters(p): Parameters<InteractParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("interact", p, ctx).await
    }

    #[tool(description = "Text buffer ranges, cursor, selection and tag spans of a text widget.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_text(&self, Parameters(p): Parameters<InspectTextParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_text", p, ctx).await
    }

    #[tool(description = "Insert, delete, replace text ranges or set the selection. Needs --allow-mutations and GTK_MCP_MUTATIONS=1 in the app.", annotations(destructive_hint = true))]
    async fn edit_text(&self, Parameters(p): Parameters<EditTextParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("edit_text", p, ctx).await
    }

    #[tool(description = "Bounded wait for existence, match count, property, text, mapped, sensitive or GApplication busy. A timeout returns satisfied:false. Emits progress notifications.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn wait_for(&self, Parameters(p): Parameters<WaitForParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("wait_for", p, ctx).await
    }

    #[tool(description = "Ordered steps (at most 100, no nesting, no lifecycle tools), all validated before the first runs; $N.ref refers to step N's target; stops at the first error unless stop_on_error is false; no rollback. Needs mutations only when some step mutates.", annotations(destructive_hint = true))]
    async fn batch(&self, Parameters(p): Parameters<BatchParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("batch", p, ctx).await
    }

    #[tool(description = "Paginated items of a list/tree model, selection, sort/filter chains.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn inspect_model(&self, Parameters(p): Parameters<InspectModelParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_model", p, ctx).await
    }

    #[tool(description = "Render a widget, window or popover to PNG via GTK (settled by default). Returns capture metadata plus the image; capture_id is retained for diff_captures.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn capture(&self, Parameters(p): Parameters<CaptureParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("capture", p, ctx).await
    }

    #[tool(description = "Pixel diff of two retained captures: changed ratio, changed regions, diff image. A size mismatch is reported explicitly and nothing is compared.", annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true))]
    async fn diff_captures(&self, Parameters(p): Parameters<DiffCapturesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("diff_captures", p, ctx).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new("gtk-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions("Inspect and exercise a running GTK4 app from inside its process. Call list_apps first; `session` may be omitted when exactly one app is running. Mutating tools need --allow-mutations on the broker and GTK_MCP_MUTATIONS=1 in the app.")
    }
}
