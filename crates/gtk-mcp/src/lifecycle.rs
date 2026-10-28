//! `launch_app`, `restart_app`, `stop_app` and the `app-state/<name>` MCP
//! resources (`docs/plan.md` 4.5, 5.5; `docs/tools.md` core / app).

use crate::broker::{Broker, ToolOutcome, lock};
use crate::launch::{self, ExitInfo, Launch, LaunchSpec};
use crate::session::Session;
use gtk_mcp_protocol::{
    AgentError, DEFAULT_LAUNCH_TIMEOUT_MS, DEFAULT_STOP_TIMEOUT_MS, ErrorCode, LaunchAppParams, RestartAppParams,
    STATE_PREFIX, SessionState, StopAppParams, state_method,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Prefix of app-state resource URIs: `gtk-mcp://<session>/app-state/<name>`.
pub const RESOURCE_SCHEME: &str = "gtk-mcp://";

/// One `app-state/<name>` resource of a running session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateResource {
    pub uri: String,
    pub session: String,
    /// Name relative to `app-state/`.
    pub name: String,
}

fn parse<T: serde::de::DeserializeOwned>(tool: &str, args: Value) -> Result<T, AgentError> {
    serde_json::from_value(args).map_err(|e| AgentError::invalid_params(format!("invalid params for '{tool}': {e}")))
}

fn ms(v: Option<u64>, default: u64, min: u64, max: u64) -> Duration {
    Duration::from_millis(v.unwrap_or(default).clamp(min, max))
}

impl Broker {
    fn require_launch(&self, tool: &str) -> Result<(), AgentError> {
        if self.policy.allow_launch {
            return Ok(());
        }
        Err(AgentError::new(
            ErrorCode::LaunchDisabled,
            format!(
                "'{tool}' manages app processes; add --allow-launch to the gtk-mcp server args in your MCP client config (e.g. the .mcp.json args array), then restart the broker"
            ),
        ))
    }

    pub(crate) async fn lifecycle(self: &Arc<Self>, tool: &str, args: Value) -> Result<ToolOutcome, AgentError> {
        self.require_launch(tool)?;
        let v = match tool {
            "launch_app" => self.launch_app(parse(tool, args)?).await?,
            "restart_app" => self.restart_app(parse(tool, args)?).await?,
            "stop_app" => self.stop_app(parse(tool, args)?).await?,
            other => return Err(AgentError::invalid_params(format!("unknown lifecycle tool '{other}'"))),
        };
        Ok(ToolOutcome::json(&v))
    }

    async fn launch_app(self: &Arc<Self>, p: LaunchAppParams) -> Result<Value, AgentError> {
        let timeout = ms(p.timeout_ms, DEFAULT_LAUNCH_TIMEOUT_MS, 100, 300_000);
        let spec = LaunchSpec::from_params(p)?;
        let session = self.start(spec, timeout).await?;
        Ok(session.listing())
    }

    /// Spawn `spec`, then wait (bounded) for the session that appears in its process group.
    async fn start(self: &Arc<Self>, spec: LaunchSpec, timeout: Duration) -> Result<Arc<Session>, AgentError> {
        let plan = spec.env_plan(&self.config.socket_dir);
        let launch = Launch::spawn(spec, &plan, self.config.ring_capacity)?;
        lock(&self.launches).push(launch.clone());
        let deadline = Instant::now() + timeout;
        loop {
            self.rescan().await;
            if let Some(s) = self.sessions().into_iter().find(|s| s.launch().is_some_and(|l| Arc::ptr_eq(l, &launch))) {
                return Ok(s);
            }
            if launch.reaped() {
                // Give the exit status (published once the output pipes drained) a moment.
                let l = launch.clone();
                let _ = tokio::task::spawn_blocking(move || l.wait_exit(Duration::from_secs(1))).await;
                return Err(launch::exited_error(
                    &launch,
                    "the app exited before its agent attached; it must embed gtk-mcp-agent and call attach",
                ));
            }
            if Instant::now() >= deadline {
                launch::stop(&launch, Duration::from_millis(500)).await;
                return Err(AgentError::new(
                    ErrorCode::Timeout,
                    format!("no agent socket appeared within {} ms; the app was stopped", timeout.as_millis()),
                )
                .with_data(json!({ "logs": launch.log_tail(20) })));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The launch behind `session`, or the refusal for apps the broker did not start.
    fn launched(session: &Session, tool: &str) -> Result<Arc<Launch>, AgentError> {
        session.launch().cloned().ok_or_else(|| {
            AgentError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "'{tool}' only acts on apps this broker launched; session {} (pid {}) was started elsewhere",
                    session.id(),
                    session.pid()
                ),
            )
        })
    }

    /// Stop `launch` and wait (bounded) until its session notices. Returns whether SIGKILL was needed.
    async fn stop_launch(&self, session: &Session, launch: &Arc<Launch>, kill_after: Duration) -> (bool, Option<ExitInfo>) {
        let forced = launch::stop(launch, kill_after).await;
        let l = launch.clone();
        let exit = tokio::task::spawn_blocking(move || l.wait_exit(Duration::from_secs(2))).await.ok().flatten();
        let end = Instant::now() + Duration::from_millis(1500);
        while session.exit_seq() == 0 && Instant::now() < end && !matches!(session.state(), SessionState::Incompatible) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (forced, exit)
    }

    async fn stop_app(self: &Arc<Self>, p: StopAppParams) -> Result<Value, AgentError> {
        let session = self.resolve_session(&p.session).await?;
        let launch = Self::launched(&session, "stop_app")?;
        let already = launch.reaped() && !launch.group_alive();
        let (forced, exit) = self.stop_launch(&session, &launch, ms(p.timeout_ms, DEFAULT_STOP_TIMEOUT_MS, 0, 60_000)).await;
        Ok(json!({
            "session": session.id(),
            "already_exited": already,
            "forced_kill": forced,
            "exit": exit.map(ExitInfo::to_json),
            "state": session.state(),
        }))
    }

    async fn restart_app(self: &Arc<Self>, p: RestartAppParams) -> Result<Value, AgentError> {
        let old = self.resolve_session(&p.session).await?;
        let launch = Self::launched(&old, "restart_app")?;
        let (forced, exit) = self.stop_launch(&old, &launch, ms(p.timeout_ms, DEFAULT_STOP_TIMEOUT_MS, 0, 60_000)).await;
        let new = self.start(launch.spec.clone(), ms(p.launch_timeout_ms, DEFAULT_LAUNCH_TIMEOUT_MS, 100, 300_000)).await?;
        Ok(json!({
            "previous": {
                "session": old.id(),
                "forced_kill": forced,
                "exit": exit.map(ExitInfo::to_json),
                "state": old.state(),
                "note": "refs and snapshot ids of this session stay with it; the new session starts with a fresh registry",
            },
            "session": new.listing(),
        }))
    }

    // ---- app-state resources ----------------------------------------------

    /// `register_state` entries of every running session. Sessions that do not
    /// answer within 3 s are skipped.
    pub async fn list_state_resources(self: &Arc<Self>) -> Vec<StateResource> {
        self.rescan().await;
        let mut out = Vec::new();
        for s in self.sessions() {
            if !matches!(s.state(), SessionState::Running) {
                continue;
            }
            let Ok(v) = s.request(state_method::LIST, json!({}), 3000, None).await else { continue };
            for name in v["states"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                out.push(StateResource {
                    uri: format!("{RESOURCE_SCHEME}{}/{STATE_PREFIX}{name}", s.id()),
                    session: s.id().to_owned(),
                    name: name.to_owned(),
                });
            }
        }
        out
    }

    /// Read one `gtk-mcp://<session>/app-state/<name>` resource; returns the registered closure's JSON value.
    pub async fn read_state_resource(self: &Arc<Self>, uri: &str) -> Result<Value, AgentError> {
        let rest = uri
            .strip_prefix(RESOURCE_SCHEME)
            .ok_or_else(|| AgentError::invalid_params(format!("unknown resource '{uri}': expected {RESOURCE_SCHEME}<session>/{STATE_PREFIX}<name>")))?;
        let (session, path) = rest.split_once('/').unwrap_or((rest, ""));
        let name = path
            .strip_prefix(STATE_PREFIX)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| AgentError::invalid_params(format!("unknown resource '{uri}': expected {RESOURCE_SCHEME}<session>/{STATE_PREFIX}<name>")))?;
        let session = self.resolve_session(session).await?;
        if let Some(e) = session.unavailable() {
            return Err(e);
        }
        let r = session.request(state_method::READ, json!({ "name": name }), self.config.default_deadline_ms, None).await?;
        Ok(r["value"].clone())
    }
}
