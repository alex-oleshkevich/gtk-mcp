//! rmcp wrappers of the `diagnostics` toolset (M5), except `object_stats`
//! (M4, in the main router). Same pattern as `actions.rs`.

use super::Server;
use gtk_mcp_protocol::*;
use rmcp::{
    ErrorData, RoleServer, handler::server::wrapper::Parameters, model::CallToolResult, service::RequestContext, tool,
    tool_router,
};

#[tool_router(router = tool_router_diagnostics, vis = "pub(super)")]
impl Server {
    #[tool(description = "Subscribe to widget events, read later with read_events: kind=notify (target, optional properties), signal (target, signal name), model (target) or atspi (app-wide; omit target; optional events prefixes such as object:state-changed:focused; needs --allow-atspi; one subscription per session, a new watch replaces it). Secret objects never report values. Returns a subscription id for unwatch.", annotations(read_only_hint = true))]
    async fn watch(&self, Parameters(p): Parameters<WatchParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("watch", p, ctx).await
    }

    #[tool(description = "Read watch events by cursor (since, limit, subscription/kind filters), including late_result records of requests that finished after their deadline. Returns next_cursor and how many events were dropped from the bounded buffer. Works after the app exited.", annotations(read_only_hint = true))]
    async fn read_events(&self, Parameters(p): Parameters<ReadEventsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("read_events", p, ctx).await
    }

    #[tool(description = "Remove a subscription made by watch.", annotations(read_only_hint = true))]
    async fn unwatch(&self, Parameters(p): Parameters<UnwatchParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("unwatch", p, ctx).await
    }

    #[tool(description = "Read captured app logs by cursor with min_level/contains/source filters: stderr/stdout of broker-launched apps (parsed into level, domain and message for GLib records) and records from gtk_mcp_agent::forward_log; unavailable otherwise. Works after the app exited.", annotations(read_only_hint = true))]
    async fn read_logs(&self, Parameters(p): Parameters<ReadLogsParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("read_logs", p, ctx).await
    }

    #[tool(description = "Measure rendering performance (GdkFrameClock) over duration_ms (default 1000, max 10000): paint_fps counts paint callbacks; presented_fps counts distinct complete compositor presentation timestamps (null without two timestamps). refresh_hz comes from reported refresh intervals, not a 60 Hz assumption. Returns paint interval statistics, missed frames, presentation timings and nonnegative latency statistics; invalid clock-domain latency samples are excluded and counted in presentation.latency_samples_discarded. An idle surface reports frames: 0.", annotations(read_only_hint = true))]
    async fn measure_frames(&self, Parameters(p): Parameters<MeasureFramesParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("measure_frames", p, ctx).await
    }

    #[tool(description = "Start recording user interactions: capture-phase input controllers on every window and popover plus emission hooks on common signals. Initializes signal classes even when the app has no instances; unavailable hooks are listed in skipped_hooks without aborting recording. Emissions within 300 ms after real input count as human, others as app (heuristic); the agent's own actions are ignored.", annotations(read_only_hint = true))]
    async fn record(&self, Parameters(p): Parameters<RecordParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("record", p, ctx).await
    }

    #[tool(description = "Stop recording and return a replayable batch script (steps with selectors: automation_id > buildable_id > type+text+ancestor, nth when ambiguous). Text typed into secret fields is {{secret:N}} (listed under secrets); app-originated emissions are listed under app_emissions.", annotations(read_only_hint = true))]
    async fn stop_recording(&self, Parameters(p): Parameters<StopRecordingParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.run("stop_recording", p, ctx).await
    }
}
