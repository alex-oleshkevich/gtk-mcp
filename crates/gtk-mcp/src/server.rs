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
    model::{
        CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams, ProgressNotificationParam, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::Serialize;
use std::sync::Arc;

mod a11y;
mod actions;
mod diagnostics;

#[derive(Clone)]
pub struct Server {
    broker: Arc<Broker>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub fn new(broker: Arc<Broker>) -> Result<Self, String> {
        let mut router = Self::tool_router() + Self::tool_router_actions() + Self::tool_router_diagnostics() + Self::tool_router_a11y();
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
        for route in router.map.values_mut() {
            let slimmed = crate::schema::slim(route.attr.input_schema.as_ref().clone());
            route.attr.input_schema = Arc::new(slimmed);
        }
        Ok(Self { broker, tool_router: router })
    }

    /// The MCP tool definitions this server lists: name, description, annotations, input schema.
    pub fn tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
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
    #[tool(description = "List running instrumented GTK4 apps (sessions) with pid, app id, GTK/libadwaita versions, capabilities and state (running, incompatible, exited). Rescans the socket directory.", annotations(read_only_hint = true))]
    async fn list_apps(&self, Parameters(p): Parameters<EmptyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_apps", p, ctx).await
    }

    #[tool(description = "List toplevel windows and mapped native surfaces of a GTK app: title, visibility, size, refs, monitor, renderer.", annotations(read_only_hint = true))]
    async fn list_windows(&self, Parameters(p): Parameters<ListWindowsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_windows", p, ctx).await
    }

    #[tool(description = "Capture the widget tree of a GTK app (view full|semantic, format text|json). Optional root is a ref or selector (target is an alias); omitted: all toplevels. Unknown parameter keys are rejected. Returns a snapshot_id retained by the broker for diff_snapshots.", annotations(read_only_hint = true))]
    async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("snapshot", p, ctx).await
    }

    #[tool(description = "Diff two retained widget-tree snapshots by snapshot_id: added/removed widgets and changed state, text, label, attached, child count and size per ref. Evicted ids return snapshot_unavailable.", annotations(read_only_hint = true))]
    async fn diff_snapshots(&self, Parameters(p): Parameters<DiffSnapshotsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("diff_snapshots", p, ctx).await
    }

    #[tool(description = "Find widgets: resolve a selector to all matching widget refs.", annotations(read_only_hint = true))]
    async fn query(&self, Parameters(p): Parameters<QueryParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("query", p, ctx).await
    }

    #[tool(description = "Inspect a widget or object: type, ancestry, identities, state (live/detached), capabilities and related objects.", annotations(read_only_hint = true))]
    async fn inspect(&self, Parameters(p): Parameters<InspectParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect", p, ctx).await
    }

    #[tool(description = "List the GObject property schemas of a widget or object, filterable by owner class or name.", annotations(read_only_hint = true))]
    async fn list_properties(&self, Parameters(p): Parameters<ListPropertiesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_properties", p, ctx).await
    }

    #[tool(description = "Read GObject property values of a widget; object values come back as refs; secret objects are redacted.", annotations(read_only_hint = true))]
    async fn get_property(&self, Parameters(p): Parameters<GetPropertyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("get_property", p, ctx).await
    }

    #[tool(description = "Write a GObject property of a widget and return the observed value.", annotations(destructive_hint = true))]
    async fn set_property(&self, Parameters(p): Parameters<SetPropertyParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("set_property", p, ctx).await
    }

    #[tool(description = "List the signal schemas of a widget or object and its watch eligibility.", annotations(read_only_hint = true))]
    async fn list_signals(&self, Parameters(p): Parameters<ListSignalsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_signals", p, ctx).await
    }

    #[tool(description = "Perform a semantic UI operation on a widget and read the state back: activate, focus, fill, set_checked, set_active, set_value, set_expanded, select, select_page, popup, popdown, scroll_to, scroll_by, close, play, pause, seek.", annotations(destructive_hint = true))]
    async fn interact(&self, Parameters(p): Parameters<InteractParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("interact", p, ctx).await
    }

    #[tool(description = "Read a text widget's buffer ranges, cursor, selection and tag spans.", annotations(read_only_hint = true))]
    async fn inspect_text(&self, Parameters(p): Parameters<InspectTextParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_text", p, ctx).await
    }

    #[tool(description = "Edit a text widget: insert, delete or replace text ranges, or set the selection.", annotations(destructive_hint = true))]
    async fn edit_text(&self, Parameters(p): Parameters<EditTextParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("edit_text", p, ctx).await
    }

    #[tool(description = "Wait (bounded) until a widget condition holds: exists, absent, match_count, property, text, mapped, sensitive or GApplication busy. A timeout returns satisfied:false. Emits progress notifications.", annotations(read_only_hint = true))]
    async fn wait_for(&self, Parameters(p): Parameters<WaitForParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("wait_for", p, ctx).await
    }

    #[tool(description = "Run ordered tool steps in one call (at most 100, no nesting, no lifecycle tools), all validated before the first runs; $N.ref refers to step N's target; stops at the first error unless stop_on_error is false; no rollback. Needs mutations only when some step mutates.", annotations(destructive_hint = true))]
    async fn batch(&self, Parameters(p): Parameters<BatchParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("batch", p, ctx).await
    }

    #[tool(description = "Page through the items of a list/tree model with its selection and sort/filter chains.", annotations(read_only_hint = true))]
    async fn inspect_model(&self, Parameters(p): Parameters<InspectModelParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_model", p, ctx).await
    }

    #[tool(description = "Render a widget, window or popover to a PNG screenshot via GTK (settled by default). After temporary CSS changes, the first settled capture per native requests a fresh frame; unchanged clean captures need no new frame. A timed-out frame is reported stale, never current. Returns capture metadata plus the image; capture_id is retained for diff_captures.", annotations(read_only_hint = true))]
    async fn capture(&self, Parameters(p): Parameters<CaptureParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("capture", p, ctx).await
    }

    #[tool(description = "Pixel-diff two retained screenshot captures: changed ratio, changed regions, diff image. A size mismatch is reported explicitly and nothing is compared.", annotations(read_only_hint = true))]
    async fn diff_captures(&self, Parameters(p): Parameters<DiffCapturesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("diff_captures", p, ctx).await
    }

    #[tool(description = "Measure a widget's layout: min/natural size (optionally at for_size), allocation, baseline, margins, alignment, expand flags, size request, layout manager, parent and sibling allocations.", annotations(read_only_hint = true))]
    async fn inspect_layout(&self, Parameters(p): Parameters<InspectLayoutParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_layout", p, ctx).await
    }

    #[tool(description = "Audit UI layout and report findings with ref and evidence: ellipsized labels, children outside parent bounds, visible zero-size widgets, overlapping siblings, unresolvable icon names. GtkInscription ellipsis is reported unsupported.", annotations(read_only_hint = true))]
    async fn audit_layout(&self, Parameters(p): Parameters<AuditLayoutParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("audit_layout", p, ctx).await
    }

    #[tool(description = "Inspect a widget's CSS name, classes, state flags and public style information (colour, font, direction). No computed CSS.", annotations(read_only_hint = true))]
    async fn inspect_style(&self, Parameters(p): Parameters<InspectStyleParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_style", p, ctx).await
    }

    #[tool(description = "Install temporary CSS on the display; parse diagnostics carry line/column (invalid CSS installs nothing). Returns a handle; reverted by remove_css or when this client disconnects. Requests a post-change frame on every mapped native, including popovers; reports settled and settled_reason (null on success, otherwise no_frame_clock, no_frame or cancelled).", annotations(destructive_hint = true))]
    async fn apply_css(&self, Parameters(p): Parameters<ApplyCssParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("apply_css", p, ctx).await
    }

    #[tool(description = "Remove one temporary CSS provider by handle. Waits for a post-change frame on every mapped native, including popovers; reports settled and settled_reason (null on success, otherwise no_frame_clock, no_frame or cancelled). Nothing mapped counts as settled; a mapped native without a clock does not.", annotations(destructive_hint = true))]
    async fn remove_css(&self, Parameters(p): Parameters<RemoveCssParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("remove_css", p, ctx).await
    }

    #[tool(description = "Outline a widget for a human (temporary CSS class gtk-mcp-highlight), clearing this client's previous highlight; omit target to just clear. Reverted on disconnect. Waits for a post-change frame on every mapped native, including popovers; reports settled and settled_reason (null on success, otherwise no_frame_clock, no_frame or cancelled).", annotations(destructive_hint = true))]
    async fn highlight(&self, Parameters(p): Parameters<HighlightParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("highlight", p, ctx).await
    }

    #[tool(description = "Dump the GSK render-node tree of a widget (type, bounds, key attributes), size-limited. Text nodes inside secret subtrees are replaced; targets containing a peeking secret are refused.", annotations(read_only_hint = true))]
    async fn inspect_render(&self, Parameters(p): Parameters<InspectRenderParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_render", p, ctx).await
    }

    #[tool(description = "Read GtkSettings values (theme, icon theme, fonts, dark preference, animations) and the libadwaita style manager when present.", annotations(read_only_hint = true))]
    async fn inspect_settings(&self, Parameters(p): Parameters<InspectSettingsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_settings", p, ctx).await
    }

    #[tool(description = "Override a desktop setting (color_scheme, animations, text_scale, text_direction) for this session; reverted on disconnect. libadwaita high contrast and accent are read-only.", annotations(destructive_hint = true))]
    async fn override_setting(&self, Parameters(p): Parameters<OverrideSettingParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("override_setting", p, ctx).await
    }

    #[tool(description = "Launch a GTK app (argv, no shell) under broker control with presets (no_portals, instance_count, a11y, language, gsk_renderer, gtk_debug), wayland_display/xdg_runtime_dir, cwd and env. Captures stdout/stderr; waits for the agent socket and returns the session. Needs --allow-launch.", annotations(destructive_hint = true))]
    async fn launch_app(&self, Parameters(p): Parameters<LaunchAppParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("launch_app", p, ctx).await
    }

    #[tool(description = "Restart a broker-launched app: SIGTERM to its process group, SIGKILL after timeout_ms (default 3000), then relaunch with the same arguments; returns the new session, the old one stays listed as exited. Needs --allow-launch; refuses apps the broker did not launch.", annotations(destructive_hint = true))]
    async fn restart_app(&self, Parameters(p): Parameters<RestartAppParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("restart_app", p, ctx).await
    }

    #[tool(description = "Stop a broker-launched app: SIGTERM to its process group, SIGKILL after timeout_ms (default 3000). Needs --allow-launch; refuses apps the broker did not launch.", annotations(destructive_hint = true))]
    async fn stop_app(&self, Parameters(p): Parameters<StopAppParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("stop_app", p, ctx).await
    }

    #[tool(description = "Open files in the app (g_application_open: paths or URIs, optional hint) or activate it (g_application_activate).", annotations(destructive_hint = true))]
    async fn app_open(&self, Parameters(p): Parameters<AppOpenParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("app_open", p, ctx).await
    }

    #[tool(description = "Read the app's GSettings: registered settings objects and schemas under its app id, with keys, types, ranges, defaults and current values; or one schema by id.", annotations(read_only_hint = true))]
    async fn inspect_gsettings(&self, Parameters(p): Parameters<InspectGsettingsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("inspect_gsettings", p, ctx).await
    }

    #[tool(description = "Write a GSettings key (JSON value converted by the key's type, or GVariant text) with type and range validation; returns the previous and observed values.", annotations(destructive_hint = true))]
    async fn set_gsetting(&self, Parameters(p): Parameters<SetGsettingParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("set_gsetting", p, ctx).await
    }

    #[tool(description = "List registered GResource paths under a path (default /), optionally recursive, with sizes.", annotations(read_only_hint = true))]
    async fn list_resources(&self, Parameters(p): Parameters<ListResourcesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("list_resources", p, ctx).await
    }

    #[tool(description = "Read one GResource as UTF-8 text or base64, size-limited (max_bytes, default 65536, at most 1048576).", annotations(read_only_hint = true))]
    async fn read_resource(&self, Parameters(p): Parameters<ReadResourceParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("read_resource", p, ctx).await
    }

    #[tool(description = "Count live GObject instances per type. Unavailable unless the app started with GOBJECT_DEBUG=instance-count (launch_app preset instance_count).", annotations(read_only_hint = true))]
    async fn object_stats(&self, Parameters(p): Parameters<ObjectStatsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("object_stats", p, ctx).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let caps = ServerCapabilities::builder().enable_tools().enable_resources().build();
        ServerConfig::new(caps)
            .with_server_info(rmcp::model::Implementation::new("gtk-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions("Inspect and exercise a running GTK4 app from inside its process. Call list_apps first; `session` may be omitted when exactly one app is running. launch_app, restart_app and stop_app need --allow-launch. State the app registered with register_state is served as resources gtk-mcp://<session>/app-state/<name>.")
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mut r = ListResourcesResult::default();
        r.resources = self
            .broker
            .list_state_resources()
            .await
            .into_iter()
            .map(|s| {
                Resource::new(s.uri, format!("{}/{STATE_PREFIX}{}", s.session, s.name))
                    .with_description(format!("App state '{}' registered by the app of session {} (register_state)", s.name, s.session))
                    .with_mime_type("application/json")
            })
            .collect();
        Ok(r)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        match self.broker.read_state_resource(&request.uri).await {
            Ok(v) => Ok(ReadResourceResult::new(vec![
                ResourceContents::text(serde_json::to_string_pretty(&v).unwrap_or_default(), request.uri).with_mime_type("application/json"),
            ])
            .into()),
            Err(e) if e.code == ErrorCode::InvalidParams || e.code == ErrorCode::NotFound => {
                Err(ErrorData::resource_not_found(e.message.clone(), serde_json::to_value(&e).ok()))
            }
            Err(e) => Err(ErrorData::internal_error(e.message.clone(), serde_json::to_value(&e).ok())),
        }
    }
}
