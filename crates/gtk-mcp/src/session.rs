//! One connection to one instrumented app: handshake, request multiplexing,
//! deadlines, notification buffers, and exit tracking (`docs/plan.md` 4.2, 5.1, 5.2).

use crate::ring::{Read, Ring, Store};
use gtk_mcp_protocol::{
    AgentError, ErrorCode, Frame, FrameReader, Hello, HelloAck, LateResult, Message, Notification, PROTOCOL_VERSION,
    Request, Response, SessionInfo, SessionState, method, parse_message, write_frame,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// A `$/progress` update from the agent for an in-flight request.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

pub type ProgressSink = Arc<dyn Fn(Progress) + Send + Sync>;

struct Pending {
    tx: oneshot::Sender<Result<Value, AgentError>>,
    /// The agent sent `$/progress {state: "started"}` for this request.
    started: bool,
    progress: Option<ProgressSink>,
}

struct Inner {
    state: SessionState,
    pending: HashMap<u64, Pending>,
    next_id: u64,
    events: Ring,
    logs: Ring,
}

pub struct Session {
    id: String,
    pid: u32,
    hello: Option<HelloAck>,
    inner: Mutex<Inner>,
    writer: Mutex<Option<UnixStream>>,
    /// 0 while alive; otherwise the order in which sessions exited (for retention).
    exit_seq: AtomicU64,
    pub(crate) snapshots: Mutex<Store<Value>>,
    pub(crate) captures: Mutex<Store<Arc<Vec<u8>>>>,
}

pub(crate) enum HandshakeError {
    /// Nothing is listening: the socket is stale.
    Refused,
    /// The socket vanished between the scan and the connect.
    Gone,
    /// Connected but no (complete) ack within the bound.
    Timeout,
    Other(String),
}

pub(crate) struct Handshake {
    pub ack: HelloAck,
    pub stream: UnixStream,
    pub reader: FrameReader<BufReader<UnixStream>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 { Err(io::Error::last_os_error()) } else { Ok(cred.uid) }
}

/// Connect, verify the peer uid, send `hello` (request id 1) and read the ack.
pub(crate) fn handshake(path: &Path, timeout: Duration) -> Result<Handshake, HandshakeError> {
    let stream = UnixStream::connect(path).map_err(|e| match e.kind() {
        io::ErrorKind::ConnectionRefused => HandshakeError::Refused,
        io::ErrorKind::NotFound => HandshakeError::Gone,
        _ => HandshakeError::Other(format!("connect: {e}")),
    })?;
    match peer_uid(&stream) {
        Ok(uid) if uid == unsafe { libc::getuid() } => {}
        Ok(uid) => return Err(HandshakeError::Other(format!("peer uid {uid} differs from ours"))),
        Err(e) => return Err(HandshakeError::Other(format!("SO_PEERCRED: {e}"))),
    }
    let io_err = |what: &str, e: io::Error| HandshakeError::Other(format!("{what}: {e}"));
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| io_err("set_write_timeout", e))?;
    let hello = Hello { protocol: PROTOCOL_VERSION, broker_version: env!("CARGO_PKG_VERSION").into() };
    let req = Request::new(1, method::HELLO, serde_json::to_value(&hello).unwrap(), None);
    write_frame(&mut &stream, &req).map_err(|e| io_err("write hello", e))?;

    let mut reader = FrameReader::new(BufReader::new(stream.try_clone().map_err(|e| io_err("clone", e))?));
    let start = Instant::now();
    loop {
        let left = timeout.checked_sub(start.elapsed()).filter(|d| !d.is_zero()).ok_or(HandshakeError::Timeout)?;
        stream.set_read_timeout(Some(left)).map_err(|e| io_err("set_read_timeout", e))?;
        let frame = match reader.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return Err(HandshakeError::Other("closed before hello_ack".into())),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Err(HandshakeError::Timeout);
            }
            Err(e) => return Err(io_err("read hello_ack", e)),
        };
        let Frame::Line(line) = frame else { continue };
        match parse_message(&line) {
            Ok(Message::Response(Response { id: Some(1), result: Some(r), .. })) => {
                let ack: HelloAck =
                    serde_json::from_value(r).map_err(|e| HandshakeError::Other(format!("bad hello_ack: {e}")))?;
                stream.set_read_timeout(None).map_err(|e| io_err("set_read_timeout", e))?;
                return Ok(Handshake { ack, stream, reader });
            }
            Ok(Message::Response(Response { id: Some(1), error: Some(e), .. })) => {
                return Err(HandshakeError::Other(format!("hello rejected: {}", e.message)));
            }
            // Notifications before the ack carry nothing we need yet.
            _ => continue,
        }
    }
}

impl Session {
    pub(crate) fn new(
        id: String,
        pid: u32,
        h: Handshake,
        ring_capacity: usize,
        retain_snapshots: usize,
        retain_captures: usize,
        exit_counter: Arc<AtomicU64>,
    ) -> Arc<Self> {
        let incompatible = h.ack.protocol != PROTOCOL_VERSION;
        let s = Arc::new(Self {
            id,
            pid,
            hello: Some(h.ack),
            inner: Mutex::new(Inner {
                state: if incompatible { SessionState::Incompatible } else { SessionState::Running },
                pending: HashMap::new(),
                next_id: 2, // 1 was the hello
                events: Ring::new(ring_capacity),
                logs: Ring::new(ring_capacity),
            }),
            writer: Mutex::new(None),
            exit_seq: AtomicU64::new(0),
            snapshots: Mutex::new(Store::new(retain_snapshots)),
            captures: Mutex::new(Store::new(retain_captures)),
        });
        if incompatible {
            // Connection closed: we can't speak to this agent. Hello stays recorded.
            drop(h.reader);
            drop(h.stream);
        } else {
            *lock(&s.writer) = Some(h.stream);
            let me = s.clone();
            let reader = h.reader;
            std::thread::Builder::new()
                .name(format!("gtk-mcp-session-{}", s.id))
                .spawn(move || reader_loop(me, reader, exit_counter))
                .expect("spawn session reader");
        }
        s
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn hello(&self) -> Option<&HelloAck> {
        self.hello.as_ref()
    }
    pub fn mutations_enabled(&self) -> bool {
        self.hello.as_ref().is_some_and(|h| h.mutations_enabled)
    }
    pub fn state(&self) -> SessionState {
        lock(&self.inner).state.clone()
    }
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            session: self.id.clone(),
            pid: self.pid,
            app_id: self.hello.as_ref().and_then(|h| h.app_id.clone()),
            launched_by_broker: false,
            state: self.state(),
            hello: self.hello.clone(),
        }
    }
    /// Ids of requests currently awaiting a response, ascending.
    pub fn in_flight(&self) -> Vec<u64> {
        let mut v: Vec<u64> = lock(&self.inner).pending.keys().copied().collect();
        v.sort_unstable();
        v
    }
    /// `$/event` params and `late_result` records with `cursor >= cursor`.
    pub fn events_since(&self, cursor: u64) -> Read {
        lock(&self.inner).events.since(cursor)
    }
    /// `$/log` params with `cursor >= cursor`.
    pub fn logs_since(&self, cursor: u64) -> Read {
        lock(&self.inner).logs.since(cursor)
    }
    pub(crate) fn exit_seq(&self) -> u64 {
        self.exit_seq.load(Ordering::Acquire)
    }

    /// Error for a session that cannot take requests, `None` when it can.
    pub(crate) fn unavailable(&self) -> Option<AgentError> {
        state_error(&lock(&self.inner).state, self.hello.as_ref().map(|h| h.protocol))
    }

    /// Send a request and await its response under `deadline_ms` (also sent as
    /// `params._meta.deadline_ms`). Never retries.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        deadline_ms: u64,
        progress: Option<ProgressSink>,
    ) -> Result<Value, AgentError> {
        let (tx, mut rx) = oneshot::channel();
        let id = {
            let mut g = lock(&self.inner);
            if let Some(e) = state_error(&g.state, self.hello.as_ref().map(|h| h.protocol)) {
                return Err(e);
            }
            let id = g.next_id;
            g.next_id += 1;
            g.pending.insert(id, Pending { tx, started: false, progress });
            id
        };
        let req = Request::new(id, method, params, Some(deadline_ms));
        if let Err(e) = self.send(&req) {
            lock(&self.inner).pending.remove(&id);
            return Err(if e.kind() == io::ErrorKind::InvalidData {
                AgentError::invalid_params(format!("request too large: {e}"))
            } else {
                AgentError::new(ErrorCode::SessionExited, format!("write to agent failed: {e}"))
            });
        }
        match tokio::time::timeout(Duration::from_millis(deadline_ms), &mut rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(AgentError::new(ErrorCode::SessionExited, "session closed before the response arrived")),
            Err(_) => {
                let removed = lock(&self.inner).pending.remove(&id);
                let Some(p) = removed else {
                    // The response raced the timer: it was already delivered.
                    return match rx.try_recv() {
                        Ok(r) => r,
                        Err(_) => Err(AgentError::new(ErrorCode::SessionExited, "session closed before the response arrived")),
                    };
                };
                let cancel = Notification::new(method::CANCEL, json!({ "id": id }));
                let _ = self.send(&cancel);
                Err(if p.started {
                    AgentError::new(
                        ErrorCode::Timeout,
                        format!("'{method}' (request {id}) started but did not finish within {deadline_ms} ms; it was cancelled"),
                    )
                    .with_data(json!({ "started": true }))
                } else {
                    AgentError::new(
                        ErrorCode::TargetUnresponsive,
                        format!("'{method}' (request {id}) was not dequeued within {deadline_ms} ms; the app's main loop is not responding"),
                    )
                    .with_data(json!({ "started": false }))
                })
            }
        }
    }

    fn send<T: serde::Serialize>(&self, msg: &T) -> io::Result<()> {
        let g = lock(&self.writer);
        match g.as_ref() {
            Some(stream) => write_frame(&mut &*stream, msg),
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "connection closed")),
        }
    }

    fn on_response(&self, r: Response) {
        let Some(id) = r.id else { return };
        let Some(p) = lock(&self.inner).pending.remove(&id) else {
            return; // dropped after a deadline; a late response is ignored
        };
        let out = match (r.result, r.error) {
            (_, Some(e)) => Err(e.into_agent_error()),
            (Some(v), None) => Ok(v),
            (None, None) => Ok(Value::Null),
        };
        let _ = p.tx.send(out);
    }

    fn on_notification(&self, n: Notification) {
        match n.method.as_str() {
            method::EVENT => {
                lock(&self.inner).events.push(n.params);
            }
            method::LOG => {
                lock(&self.inner).logs.push(n.params);
            }
            method::LATE_RESULT => match serde_json::from_value::<LateResult>(n.params) {
                Ok(l) => {
                    lock(&self.inner).events.push(json!({ "kind": "late_result", "id": l.id, "result": l.result }));
                }
                Err(e) => eprintln!("gtk-mcp: session {}: bad $/late_result: {e}", self.id),
            },
            method::PROGRESS => self.on_progress(n.params),
            other => eprintln!("gtk-mcp: session {}: ignoring notification {other}", self.id),
        }
    }

    fn on_progress(&self, params: Value) {
        let Some(id) = params.get("id").and_then(Value::as_u64) else { return };
        if params.get("state").and_then(Value::as_str) == Some("started") {
            if let Some(p) = lock(&self.inner).pending.get_mut(&id) {
                p.started = true;
            }
            return;
        }
        let Some(progress) = params.get("progress").and_then(Value::as_f64) else { return };
        let sink = lock(&self.inner).pending.get(&id).and_then(|p| p.progress.clone());
        if let Some(sink) = sink {
            sink(Progress {
                progress,
                total: params.get("total").and_then(Value::as_f64),
                message: params.get("message").and_then(Value::as_str).map(str::to_owned),
            });
        }
    }

    fn mark_exited(&self, seq: u64) {
        let (pending, state) = {
            let mut g = lock(&self.inner);
            let in_flight = g.pending.keys().min().copied();
            g.state = SessionState::Exited { code: None, signal: None, in_flight };
            (std::mem::take(&mut g.pending), g.state.clone())
        };
        self.exit_seq.store(seq, Ordering::Release);
        if let Some(w) = lock(&self.writer).take() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
        let err = state_error(&state, None).expect("exited state is an error");
        for (_, p) in pending {
            let _ = p.tx.send(Err(err.clone()));
        }
    }
}

fn state_error(state: &SessionState, agent_protocol: Option<u32>) -> Option<AgentError> {
    match state {
        SessionState::Running => None,
        SessionState::Incompatible => Some(
            AgentError::new(
                ErrorCode::VersionMismatch,
                format!(
                    "agent speaks protocol {}, this broker speaks {PROTOCOL_VERSION}",
                    agent_protocol.map_or("?".into(), |p| p.to_string())
                ),
            )
            .with_data(json!({ "agent_protocol": agent_protocol, "broker_protocol": PROTOCOL_VERSION })),
        ),
        SessionState::Exited { code, signal, in_flight } => Some(
            AgentError::new(ErrorCode::SessionExited, "the app's session has exited")
                .with_data(json!({ "code": code, "signal": signal, "in_flight": in_flight })),
        ),
    }
}

fn reader_loop(s: Arc<Session>, mut reader: FrameReader<BufReader<UnixStream>>, exit_counter: Arc<AtomicU64>) {
    loop {
        match reader.next_frame() {
            Ok(Some(Frame::Line(line))) => match parse_message(&line) {
                Ok(Message::Response(r)) => s.on_response(r),
                Ok(Message::Notification(n)) => s.on_notification(n),
                Ok(Message::Request(r)) => {
                    eprintln!("gtk-mcp: session {}: ignoring agent request '{}'", s.id, r.method)
                }
                Err((id, e)) => {
                    eprintln!("gtk-mcp: session {}: {e}", s.id);
                    if let Some(p) = id.and_then(|id| lock(&s.inner).pending.remove(&id)) {
                        let _ = p.tx.send(Err(AgentError::invalid_params(e.to_string())));
                    }
                }
            },
            Ok(Some(Frame::Oversized { len })) => {
                eprintln!("gtk-mcp: session {}: dropped oversized frame ({len} bytes)", s.id)
            }
            Ok(None) | Err(_) => break,
        }
    }
    s.mark_exited(exit_counter.fetch_add(1, Ordering::AcqRel) + 1);
}
