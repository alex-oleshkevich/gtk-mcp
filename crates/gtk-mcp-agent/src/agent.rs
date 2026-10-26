//! The agent: main-thread state, request context and the executor loop
//! (plan 4.2, 5.2).

use crate::config::Config;
use crate::registry::Registry;
use crate::socket::{Conn, Incoming, Req, SocketHandle};
use crate::{layer, tools};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{
    AgentError, Capability, ErrorCode, Hello, HelloAck, PROTOCOL_VERSION, method, tool_spec,
};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

thread_local! {
    static AGENT: RefCell<Option<Rc<Agent>>> = const { RefCell::new(None) };
}

pub struct Agent {
    pub config: Config,
    pub registry: Registry,
    pub app: gtk::Application,
    /// `register_state` closures and registered `GSettings` objects.
    pub app_state: crate::state::AppState,
    snapshot_seq: Cell<u64>,
    capture_seq: Cell<u64>,
    socket: RefCell<Option<SocketHandle>>,
}

impl Agent {
    pub fn new(app: gtk::Application, config: Config) -> Rc<Self> {
        Rc::new(Self {
            config,
            registry: Registry::default(),
            app,
            app_state: Default::default(),
            snapshot_seq: Cell::new(0),
            capture_seq: Cell::new(0),
            socket: RefCell::new(None),
        })
    }

    pub fn install(self: &Rc<Self>) {
        AGENT.with(|a| *a.borrow_mut() = Some(self.clone()));
    }

    /// The agent attached to this (main) thread, if any.
    pub fn current() -> Option<Rc<Agent>> {
        AGENT.with(|a| a.borrow().clone())
    }

    pub fn set_socket(&self, h: SocketHandle) {
        *self.socket.borrow_mut() = Some(h);
    }

    pub fn socket_path(&self) -> Option<std::path::PathBuf> {
        self.socket.borrow().as_ref().map(|s| s.path.clone())
    }

    pub fn cleanup(&self) {
        if let Some(s) = self.socket.borrow_mut().take() {
            s.cleanup();
        }
    }

    pub fn next_snapshot_seq(&self) -> u64 {
        self.snapshot_seq.set(self.snapshot_seq.get() + 1);
        self.snapshot_seq.get()
    }

    pub fn next_capture_seq(&self) -> u64 {
        self.capture_seq.set(self.capture_seq.get() + 1);
        self.capture_seq.get()
    }

    fn on_disconnect(&self, _conn: u64) {
        // Per-connection state (temporary CSS, watches, recordings) arrives with
        // later milestones and is released here.
    }

    pub fn hello_ack(&self) -> HelloAck {
        let mut caps = Vec::new();
        let adw = adw_version();
        if adw.is_some() || glib::Type::from_name("AdwStyleManager").is_some() {
            caps.push(Capability::Adwaita);
        }
        if layer::loaded() {
            caps.push(Capability::LayerShell);
        }
        if sym_present(c"gtk_accessible_get_accessible_id") {
            caps.push(Capability::AccessibleId);
        }
        if self.config.instance_count {
            caps.push(Capability::InstanceCount);
        }
        HelloAck {
            protocol: PROTOCOL_VERSION,
            agent_version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            app_id: self.app.application_id().map(Into::into),
            gtk_version: [gtk::major_version(), gtk::minor_version(), gtk::micro_version()],
            adw_version: adw,
            capabilities: caps,
            mutations_enabled: self.config.mutations,
        }
    }
}

/// gtk-rs keeps its own "initialized" flag. Hosts that initialized GTK through
/// C / GI leave it unset: read the C-side flag and set the gtk-rs one.
/// Returns whether GTK is initialized at all.
pub fn ensure_gtk_rs_initialized() -> bool {
    if gtk::is_initialized() {
        return true;
    }
    // SAFETY: plain query of GTK's own flag.
    if unsafe { gtk::ffi::gtk_is_initialized() } != 0 {
        // SAFETY: GTK is initialized and requests run on the main thread.
        unsafe { gtk::set_initialized() };
        return true;
    }
    false
}

fn sym_present(name: &std::ffi::CStr) -> bool {
    // SAFETY: plain lookup in the global namespace.
    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }.is_null()
}

fn adw_version() -> Option<[u32; 3]> {
    let get = |name: &std::ffi::CStr| -> Option<u32> {
        // SAFETY: libadwaita's version getters are `guint f(void)`.
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let f: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(p) };
        Some(unsafe { f() })
    };
    Some([get(c"adw_get_major_version")?, get(c"adw_get_minor_version")?, get(c"adw_get_micro_version")?])
}

/// Per-request context handed to handlers.
#[derive(Clone)]
pub struct Ctx {
    pub id: u64,
    pub conn: Conn,
    pub cancelled: Arc<AtomicBool>,
    pub deadline: Option<Instant>,
    pub started: Instant,
}

impl Ctx {
    /// Cancellation / deadline check for handlers' await points.
    pub fn check(&self) -> Result<(), AgentError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(AgentError::new(ErrorCode::Timeout, "request cancelled"));
        }
        Ok(())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// A child context for a batch step with its own deadline.
    pub fn with_deadline(&self, deadline: Option<Instant>) -> Ctx {
        Ctx { deadline, ..self.clone() }
    }

    pub fn progress(&self, progress: f64, total: Option<f64>, message: &str) {
        self.conn.notify(
            method::PROGRESS,
            json!({"id": self.id, "progress": progress, "total": total, "message": message}),
        );
    }
}

/// Run a tool by name (also used by `batch` for its steps).
pub fn run_tool<'a>(agent: &'a Rc<Agent>, tool: &'a str, params: Value, ctx: &'a Ctx) -> BoxFut<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let spec = tool_spec(tool)
            .filter(|s| s.agent)
            .ok_or_else(|| AgentError::new(ErrorCode::NotFound, format!("unknown method '{tool}'")))?;
        if spec.mutating && tool != "batch" && !agent.config.mutations {
            return Err(AgentError::new(
                ErrorCode::MutationsDisabled,
                "this app was started without GTK_MCP_MUTATIONS=1",
            ));
        }
        ctx.check()?;
        tools::dispatch(agent, tool, params, ctx).await
    })
}

async fn handle(agent: Rc<Agent>, req: Req) {
    let ctx = Ctx {
        id: req.id,
        conn: req.conn.clone(),
        cancelled: req.cancelled.clone(),
        deadline: req.deadline,
        started: Instant::now(),
    };
    if req.method == method::HELLO {
        let res = match serde_json::from_value::<Hello>(req.params.clone()) {
            Ok(_) => Ok(serde_json::to_value(agent.hello_ack()).expect("HelloAck serializes")),
            Err(e) => Err(AgentError::invalid_params(format!("invalid hello: {e}"))),
        };
        req.conn.respond(req.id, res);
        return;
    }
    if let Some(res) = crate::state::handle(&agent, &req.method, &req.params) {
        req.conn.respond(req.id, res);
        return;
    }
    if !ensure_gtk_rs_initialized() {
        req.conn.respond(req.id, Err(AgentError::new(ErrorCode::Unavailable, "GTK is not initialized yet")));
        return;
    }
    let mut params = req.params.clone();
    if let Some(o) = params.as_object_mut() {
        o.remove("_meta");
    }
    let mutating = tool_spec(&req.method).is_some_and(|s| s.mutating);
    let result = run_tool(&agent, &req.method, params, &ctx).await;
    if ctx.is_cancelled() {
        // The broker has already answered this call. A mutation that did run is
        // reported as a late result so it is recorded, never retried.
        if mutating {
            if let Ok(v) = result {
                req.conn.notify(method::LATE_RESULT, json!({"id": req.id, "result": v}));
            }
        }
        req.conn.forget(req.id);
        return;
    }
    req.conn.respond(req.id, result);
}

/// The executor loop: runs on the default main context; serves requests only
/// while the main loop iterates. Requests that arrived earlier wait in the
/// channel (plan 4.2; spike S1).
pub async fn executor(agent: Rc<Agent>, rx: async_channel::Receiver<Incoming>) {
    while let Ok(incoming) = rx.recv().await {
        match incoming {
            Incoming::Disconnect(conn) => agent.on_disconnect(conn),
            Incoming::Request(req) => {
                let expired = req.deadline.is_some_and(|d| Instant::now() >= d);
                if req.cancelled.load(Ordering::SeqCst) || expired {
                    let msg = if expired { "deadline passed before the request was dequeued" } else { "cancelled before it started" };
                    req.conn.respond(
                        req.id,
                        Err(AgentError::new(ErrorCode::NotStarted, msg).with_data(json!({"started": false}))),
                    );
                    continue;
                }
                req.conn.notify(method::PROGRESS, json!({"id": req.id, "state": "started"}));
                glib::spawn_future_local(handle(agent.clone(), req));
            }
        }
    }
}
