//! The broker: session discovery, policy, and tool dispatch. Free of MCP types;
//! `server.rs` wraps [`Broker::call_tool`] for rmcp.

use crate::diff;
use crate::launch::{self, Launch};
use crate::policy::{Config, Policy};
use crate::session::{HandshakeError, ProgressSink, Session, handshake};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gtk_mcp_protocol::{
    AgentError, BatchParams, CaptureParams, DEFAULT_FRAME_WINDOW_MS, DEFAULT_SETTLE_TIMEOUT_MS, DiffCapturesParams,
    DiffSnapshotsParams, ErrorCode, MAX_FRAME_WINDOW_MS, MAX_SETTLE_TIMEOUT_MS, MeasureFramesParams, SessionInfo,
    SessionState, SnapshotFormat, SnapshotParams, ToolSpec, WaitForParams, parse_params, snapshot, tool_spec,
    validate_step,
};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub const MAX_BATCH_STEPS: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    /// `image/png` bytes.
    Png(Vec<u8>),
}

/// A successful tool result: one or more contents (text first).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub contents: Vec<Content>,
}

impl ToolOutcome {
    fn text(s: impl Into<String>) -> Self {
        Self { contents: vec![Content::Text(s.into())] }
    }
    pub(crate) fn json(v: &Value) -> Self {
        Self::text(serde_json::to_string_pretty(v).unwrap_or_default())
    }
    /// The first text content.
    pub fn text_content(&self) -> &str {
        self.contents
            .iter()
            .find_map(|c| if let Content::Text(t) = c { Some(t.as_str()) } else { None })
            .unwrap_or("")
    }
    /// The first text content parsed as JSON.
    pub fn json_content(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(self.text_content())
    }
    /// PNG images, in order.
    pub fn images(&self) -> Vec<&[u8]> {
        self.contents.iter().filter_map(|c| if let Content::Png(p) = c { Some(p.as_slice()) } else { None }).collect()
    }
}

pub struct Broker {
    pub(crate) policy: Policy,
    pub(crate) config: Config,
    enabled: HashSet<&'static str>,
    pub(crate) sessions: Mutex<Vec<Arc<Session>>>,
    /// Apps this broker launched (`launch.rs`); each is adopted by the session whose socket appears in its process group.
    pub(crate) launches: Mutex<Vec<Arc<Launch>>>,
    next_session: AtomicU64,
    exit_counter: Arc<AtomicU64>,
    scan_lock: Mutex<()>,
    /// Shared AT-SPI connection (`--allow-atspi`), created on first use.
    pub(crate) atspi: tokio::sync::Mutex<Option<crate::a11y::atspi::Atspi>>,
    /// Running AT-SPI event forwarders by session id.
    pub(crate) atspi_watches: Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn invalid(msg: impl Into<String>) -> AgentError {
    AgentError::invalid_params(msg)
}

fn pid_alive(pid: u32) -> bool {
    let Ok(p) = i32::try_from(pid) else { return false };
    if p == 0 {
        return false;
    }
    // EPERM means the process exists but belongs to someone else.
    unsafe { libc::kill(p, 0) == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) }
}

impl Broker {
    /// Fails when the policy names unknown toolsets or tools.
    pub fn new(policy: Policy, config: Config) -> Result<Arc<Self>, String> {
        let enabled = policy.enabled_tools()?.into_iter().map(|t| t.name).collect();
        Ok(Arc::new(Self {
            policy,
            config,
            enabled,
            sessions: Mutex::new(Vec::new()),
            launches: Mutex::new(Vec::new()),
            next_session: AtomicU64::new(1),
            exit_counter: Arc::new(AtomicU64::new(0)),
            scan_lock: Mutex::new(()),
            atspi: tokio::sync::Mutex::new(None),
            atspi_watches: Mutex::new(Default::default()),
        }))
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn is_enabled(&self, tool: &str) -> bool {
        self.enabled.contains(tool)
    }
    pub fn enabled_tools(&self) -> Vec<&'static ToolSpec> {
        gtk_mcp_protocol::TOOLS.iter().filter(|t| self.is_enabled(t.name)).collect()
    }

    /// Known sessions in creation order (running, incompatible, retained exited).
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        let mut g = lock(&self.sessions);
        let mut exited: Vec<(u64, String)> =
            g.iter().filter(|s| s.exit_seq() != 0).map(|s| (s.exit_seq(), s.id().to_owned())).collect();
        if exited.len() > self.config.retain_exited {
            exited.sort();
            let drop_n = exited.len() - self.config.retain_exited;
            let drop_ids: HashSet<String> = exited.into_iter().take(drop_n).map(|(_, id)| id).collect();
            g.retain(|s| !drop_ids.contains(s.id()));
        }
        // A finished launch is only kept while a session still refers to it.
        lock(&self.launches).retain(|l| !l.reaped() || g.iter().any(|s| s.launch().is_some_and(|sl| Arc::ptr_eq(sl, l))));
        g.clone()
    }

    /// The unclaimed launch whose process group contains `pid`, claimed for its session.
    fn adopt_launch(&self, pid: u32) -> Option<Arc<Launch>> {
        // SAFETY: plain query; -1 when the process is gone.
        let pgid = unsafe { libc::getpgid(pid as i32) };
        if pgid <= 0 {
            return None;
        }
        lock(&self.launches).iter().find(|l| l.pid() as i32 == pgid && l.claim()).cloned()
    }

    /// Terminate every app this broker still runs (SIGTERM, SIGKILL after 500 ms).
    /// Also done when the broker is dropped.
    pub fn shutdown(&self) {
        let all = lock(&self.launches).clone();
        launch::terminate_all(&all);
    }

    pub fn session_infos(&self) -> Vec<SessionInfo> {
        self.sessions().iter().map(|s| s.info()).collect()
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions().into_iter().find(|s| s.id() == id)
    }

    /// Scan the socket directory, drop stale sockets, connect to new ones.
    pub async fn rescan(self: &Arc<Self>) {
        let b = self.clone();
        let _ = tokio::task::spawn_blocking(move || b.rescan_blocking()).await;
    }

    /// Blocking form of [`Broker::rescan`].
    ///
    /// A socket whose handshake times out is neither listed nor unlinked: the
    /// process is alive and may just be busy at startup, so the next rescan
    /// retries it. A hung app therefore delays one scan by at most the handshake
    /// timeout and never appears as a session it cannot serve.
    pub fn rescan_blocking(&self) {
        let _scan = lock(&self.scan_lock);
        let Ok(dir) = std::fs::read_dir(&self.config.socket_dir) else { return };
        let mut found: Vec<(u32, std::path::PathBuf)> = dir
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let pid: u32 = name.strip_suffix(".sock")?.parse().ok()?;
                Some((pid, e.path()))
            })
            .collect();
        found.sort();
        for (pid, path) in found {
            if !pid_alive(pid) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            // A known session blocks only its own process: after pid reuse, a
            // retained (exited or incompatible) session must not hide a new app.
            let start_time = crate::session::proc_start_time(pid);
            if lock(&self.sessions).iter().any(|s| s.is_process(pid, start_time)) {
                continue;
            }
            match handshake(&path, pid, self.config.handshake_timeout) {
                Ok(h) => {
                    let id = format!("s{}", self.next_session.fetch_add(1, Ordering::Relaxed));
                    let launch = self.adopt_launch(pid);
                    let s = Session::new(
                        id,
                        pid,
                        h,
                        self.config.ring_capacity,
                        self.config.retain_snapshots,
                        self.config.retain_captures,
                        self.exit_counter.clone(),
                        launch,
                    );
                    lock(&self.sessions).push(s);
                }
                Err(HandshakeError::Refused) => unlink_stale(&path),
                Err(HandshakeError::Gone) => {}
                Err(HandshakeError::Timeout) => {
                    eprintln!("gtk-mcp: {}: no hello_ack within {:?}; skipping", path.display(), self.config.handshake_timeout)
                }
                Err(HandshakeError::Other(e)) => eprintln!("gtk-mcp: {}: handshake failed: {e}", path.display()),
            }
        }
    }

    /// `session` param to a session: a named session, or the only running one.
    /// A miss rescans once before failing.
    pub async fn resolve_session(self: &Arc<Self>, name: &str) -> Result<Arc<Session>, AgentError> {
        for attempt in 0..2 {
            let list = self.sessions();
            if name.is_empty() {
                let running: Vec<&Arc<Session>> =
                    list.iter().filter(|s| matches!(s.state(), SessionState::Running)).collect();
                if running.len() == 1 {
                    return Ok(running[0].clone());
                }
                if running.len() > 1 || attempt == 1 {
                    return Err(invalid(format!(
                        "no `session` given and {} sessions are running; pass one of: {}",
                        running.len(),
                        describe(&list)
                    )));
                }
            } else if let Some(s) = list.iter().find(|s| s.id() == name) {
                return Ok(s.clone());
            } else if attempt == 1 {
                return Err(invalid(format!("unknown session '{name}'; sessions: {}", describe(&list))));
            }
            self.rescan().await;
        }
        unreachable!()
    }

    pub async fn call_tool(self: &Arc<Self>, name: &str, args: Value) -> Result<ToolOutcome, AgentError> {
        self.call_tool_with_progress(name, args, None).await
    }

    /// Execute one tool. `progress` receives agent `$/progress` updates for the call.
    pub async fn call_tool_with_progress(
        self: &Arc<Self>,
        name: &str,
        args: Value,
        progress: Option<ProgressSink>,
    ) -> Result<ToolOutcome, AgentError> {
        let spec = tool_spec(name).filter(|_| self.is_enabled(name)).ok_or_else(|| invalid(format!("unknown tool '{name}'")))?;
        let mut args = match args {
            Value::Null => Value::Object(Map::new()),
            v @ Value::Object(_) => v,
            _ => return Err(invalid(format!("arguments of '{name}' must be an object"))),
        };
        {
            let o = args.as_object_mut().unwrap();
            if o.get("session").is_none_or(Value::is_null) {
                o.insert("session".into(), Value::String(String::new()));
            }
        }
        parse_params(name, args.clone()).map_err(|e| invalid(format!("invalid params for '{name}': {e}")))?;
        let session_arg = args["session"].as_str().unwrap_or_default().to_owned();

        if name == "list_apps" {
            self.rescan().await;
            return Ok(ToolOutcome::json(&Value::Array(self.sessions().iter().map(|s| s.listing()).collect())));
        }
        if matches!(name, "launch_app" | "restart_app" | "stop_app") {
            return self.lifecycle(name, args).await;
        }

        // Everything that can be wrong with the params is wrong before anything is sent.
        let mutating = self.classify(spec, &args)?;
        let session = self.resolve_session(&session_arg).await?;

        match name {
            "diff_snapshots" => return self.diff_snapshots(&session, args),
            "diff_captures" => return self.diff_captures(&session, args),
            "read_events" => {
                let p = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
                return crate::diagnostics::read_events(&session, p);
            }
            "read_logs" => {
                let p = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
                return crate::diagnostics::read_logs(&session, p);
            }
            _ => {}
        }
        if name == "unwatch" && args["subscription"] == "atspi" {
            if !self.unwatch_atspi(session.id()) {
                return Err(AgentError::new(ErrorCode::NotFound, "no AT-SPI subscription for this session"));
            }
            return Ok(ToolOutcome::json(&serde_json::json!({"subscription": "atspi", "removed": true})));
        }
        if let Some(e) = session.unavailable() {
            return Err(e);
        }
        if mutating {
            if !self.policy.allow_mutations {
                return Err(AgentError::new(
                    ErrorCode::MutationsDisabled,
                    format!("'{name}' mutates the app; start the broker with --allow-mutations"),
                ));
            }
            if !session.mutations_enabled() {
                return Err(AgentError::new(
                    ErrorCode::MutationsDisabled,
                    format!("'{name}' mutates the app; start session {} with GTK_MCP_MUTATIONS=1", session.id()),
                ));
            }
        }
        if name == "watch" && args["kind"] == "atspi" {
            let p: gtk_mcp_protocol::WatchParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
            if p.target.is_some() {
                return Err(invalid("AT-SPI watches are app-wide; omit target"));
            }
            let gtk_mcp_protocol::WatchKind::Atspi { events } = p.watch else { unreachable!() };
            return self.watch_atspi(&session, events).await.map(|v| ToolOutcome::json(&v));
        }

        if matches!(name, "accessible_tree" | "inspect_accessible" | "audit_accessibility") {
            args.as_object_mut().unwrap().insert("session".into(), Value::String(session.id().to_owned()));
            return self.a11y(name, &session, args, progress).await;
        }

        let deadline_ms = self.deadline_ms(name, &args);
        args.as_object_mut().unwrap().insert("session".into(), Value::String(session.id().to_owned()));
        let result = session.request(name, args.clone(), deadline_ms, progress).await?;
        self.finish(name, &session, &args, result)
    }

    /// Whether the call mutates; for `batch`, validates every step first.
    fn classify(&self, spec: &ToolSpec, args: &Value) -> Result<bool, AgentError> {
        if spec.name != "batch" {
            return Ok(spec.mutating);
        }
        let p: BatchParams =
            serde_json::from_value(args.clone()).map_err(|e| invalid(format!("invalid params for 'batch': {e}")))?;
        if p.steps.is_empty() {
            return Err(invalid("batch needs at least one step"));
        }
        if p.steps.len() > MAX_BATCH_STEPS {
            return Err(invalid(format!("batch has {} steps; the maximum is {MAX_BATCH_STEPS}", p.steps.len())));
        }
        let mut mutating = false;
        // Steps are numbered from 1, as in the agent's validation and `$N.ref`.
        for (n, step) in (1..).zip(&p.steps) {
            let at = |msg: String| invalid(format!("batch step {n}: {msg}")).with_data(serde_json::json!({"step": n}));
            let s = validate_step(&step.tool, &step.arguments).map_err(at)?;
            if !self.is_enabled(s.name) {
                return Err(at(format!("tool '{}' is not enabled", s.name)));
            }
            mutating |= s.mutating;
        }
        Ok(mutating)
    }

    fn deadline_ms(&self, name: &str, args: &Value) -> u64 {
        match name {
            "wait_for" => {
                let t = serde_json::from_value::<WaitForParams>(args.clone()).ok().and_then(|p| p.timeout_ms);
                t.unwrap_or(5000).min(60_000) + 2000
            }
            "capture" => {
                let t = serde_json::from_value::<CaptureParams>(args.clone()).ok().and_then(|p| p.settle_timeout_ms);
                t.unwrap_or(DEFAULT_SETTLE_TIMEOUT_MS).min(MAX_SETTLE_TIMEOUT_MS) + 5000
            }
            "batch" => 60_000,
            "measure_frames" => {
                let t = serde_json::from_value::<MeasureFramesParams>(args.clone()).ok().and_then(|p| p.duration_ms);
                t.unwrap_or(DEFAULT_FRAME_WINDOW_MS).min(MAX_FRAME_WINDOW_MS) + 5000
            }
            _ => self.config.default_deadline_ms,
        }
    }

    /// Turn an agent result into the tool outcome, retaining snapshots and captures.
    fn finish(&self, name: &str, session: &Session, args: &Value, mut result: Value) -> Result<ToolOutcome, AgentError> {
        if name == "snapshot" {
            return self.finish_snapshot(session, args, result);
        }
        let mut pngs = Vec::new();
        extract_pngs(&mut result, session, &mut pngs)?;
        let mut contents = vec![Content::Text(serde_json::to_string_pretty(&result).unwrap_or_default())];
        contents.extend(pngs.into_iter().map(Content::Png));
        Ok(ToolOutcome { contents })
    }

    fn finish_snapshot(&self, session: &Session, args: &Value, mut result: Value) -> Result<ToolOutcome, AgentError> {
        let bad = |m: &str| AgentError::new(ErrorCode::Unavailable, format!("agent returned a malformed snapshot: {m}"));
        let obj = result.as_object_mut().ok_or_else(|| bad("not an object"))?;
        let id = obj.get("snapshot_id").and_then(Value::as_str).ok_or_else(|| bad("missing snapshot_id"))?.to_owned();
        obj.insert("session".into(), Value::String(session.id().to_owned()));
        lock(&session.snapshots).insert(id.clone(), result.clone());
        let p: SnapshotParams = serde_json::from_value(args.clone()).map_err(|e| invalid(e.to_string()))?;
        match p.format.unwrap_or_default() {
            SnapshotFormat::Json => Ok(ToolOutcome::json(&result)),
            SnapshotFormat::Text => {
                let mut text = snapshot::render_text(&result, p.text_limit.unwrap_or(128) as usize);
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&format!("snapshot_id: {id}"));
                Ok(ToolOutcome::text(text))
            }
        }
    }

    fn diff_snapshots(&self, session: &Session, args: Value) -> Result<ToolOutcome, AgentError> {
        let p: DiffSnapshotsParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
        let store = lock(&session.snapshots);
        let get = |id: &str| {
            store.get(id).ok_or_else(|| {
                AgentError::new(
                    ErrorCode::SnapshotUnavailable,
                    format!("snapshot '{id}' is unknown or was evicted (retained per session: --retain-snapshots)"),
                )
            })
        };
        let (a, b) = (get(&p.from)?, get(&p.to)?);
        Ok(ToolOutcome::json(&diff::diff_snapshots(&p.from, a, &p.to, b)))
    }

    fn diff_captures(&self, session: &Session, args: Value) -> Result<ToolOutcome, AgentError> {
        let p: DiffCapturesParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
        let (a, b) = {
            let store = lock(&session.captures);
            let get = |id: &str| {
                store.get(id).cloned().ok_or_else(|| {
                    AgentError::new(
                        ErrorCode::SnapshotUnavailable,
                        format!("capture '{id}' is unknown or was evicted (retained per session: --retain-captures)"),
                    )
                })
            };
            (get(&p.from)?, get(&p.to)?)
        };
        let d = diff::diff_captures(&p.from, &a, &p.to, &b)
            .map_err(|e| AgentError::new(ErrorCode::Unavailable, format!("cannot decode retained PNG: {e}")))?;
        let mut contents = vec![Content::Text(serde_json::to_string_pretty(&d.report).unwrap_or_default())];
        contents.extend(d.image.map(Content::Png));
        Ok(ToolOutcome { contents })
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        for (_, h) in lock(&self.atspi_watches).drain() {
            h.abort();
        }
        self.shutdown();
    }
}

fn unlink_stale(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn describe(list: &[Arc<Session>]) -> String {
    if list.is_empty() {
        return "none".into();
    }
    list.iter()
        .map(|s| {
            let i = s.info();
            let state = match i.state {
                SessionState::Running => "running",
                SessionState::Incompatible => "incompatible",
                SessionState::Exited { .. } => "exited",
            };
            format!("{} (pid {}, app_id {}, {state})", i.session, i.pid, i.app_id.as_deref().unwrap_or("-"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Strip every `png_base64` the agent embedded (a `capture` result, or captures
/// inside `batch` steps and `observe` results), retain the PNG under its
/// `capture_id`, and collect the bytes for image content.
fn extract_pngs(v: &mut Value, session: &Session, out: &mut Vec<Vec<u8>>) -> Result<(), AgentError> {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(b64)) = m.get("png_base64") {
                let bytes = STANDARD.decode(b64.as_bytes()).map_err(|e| {
                    AgentError::new(ErrorCode::CaptureUnavailable, format!("agent returned invalid base64 PNG: {e}"))
                })?;
                m.remove("png_base64");
                let bytes = Arc::new(bytes);
                if let Some(id) = m.get("capture_id").and_then(Value::as_str) {
                    lock(&session.captures).insert(id.to_owned(), bytes.clone());
                }
                out.push(bytes.as_ref().clone());
            }
            for c in m.values_mut() {
                extract_pngs(c, session, out)?;
            }
        }
        Value::Array(a) => {
            for c in a {
                extract_pngs(c, session, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}
