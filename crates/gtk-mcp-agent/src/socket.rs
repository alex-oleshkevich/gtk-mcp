//! Socket thread: bind, peer check, NDJSON framing (plan 5.1, embedding.md).
//!
//! Nothing here touches GTK. Only `Send` data (JSON values, bytes, flags)
//! crosses to the main-thread executor through an `async_channel`.

use gtk_mcp_protocol::{
    AgentError, ErrorCode, Frame, FrameReader, Message, Notification, Response, method, parse_message, write_frame,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// A connection's outbound half. Cloneable and `Send`; the writer thread owns
/// the stream.
#[derive(Clone)]
pub struct Conn {
    pub id: u64,
    tx: mpsc::Sender<Vec<u8>>,
    inflight: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    /// Set by the read thread when the peer is gone, before `Disconnect` is queued.
    closed: Arc<AtomicBool>,
}

impl Conn {
    fn send_line(&self, msg: &impl serde::Serialize) {
        let mut buf = Vec::new();
        match write_frame(&mut buf, msg) {
            Ok(()) => {
                let _ = self.tx.send(buf);
            }
            Err(e) => {
                // Too large to send: tell the peer instead of dropping silently.
                let value = serde_json::to_value(msg).unwrap_or(Value::Null);
                let id = value.get("id").and_then(Value::as_u64);
                if id.is_some() {
                    let mut b = Vec::new();
                    let r = Response::err(id, AgentError::new(ErrorCode::InvalidValue, format!("response not sendable: {e}")));
                    if write_frame(&mut b, &r).is_ok() {
                        let _ = self.tx.send(b);
                    }
                }
            }
        }
    }

    pub fn respond(&self, id: u64, result: Result<Value, AgentError>) {
        self.inflight.lock().unwrap().remove(&id);
        let r = match result {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(Some(id), e),
        };
        self.send_line(&r);
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send_line(&Notification::new(method, params));
    }

    /// Forget a request that will not be answered (cancelled, late result sent).
    pub fn forget(&self, id: u64) {
        self.inflight.lock().unwrap().remove(&id);
    }

    /// The peer disconnected: nothing sent here is read any more.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl Conn {
    /// A connection whose peer is gone (unit tests of handlers).
    pub(crate) fn detached(id: u64) -> Self {
        Conn { id, tx: mpsc::channel().0, inflight: Arc::default(), closed: Arc::new(AtomicBool::new(true)) }
    }

    /// An open connection and the frames written to it (unit tests).
    pub(crate) fn open(id: u64) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        (Conn { id, tx, inflight: Arc::default(), closed: Arc::default() }, rx)
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub struct Req {
    pub conn: Conn,
    pub id: u64,
    pub method: String,
    pub params: Value,
    pub received: Instant,
    pub deadline: Option<Instant>,
    /// Set by the socket thread on `$/cancel` or disconnect, so cancellation is
    /// effective even while the main loop is blocked.
    pub cancelled: Arc<AtomicBool>,
}

pub enum Incoming {
    Request(Req),
    Disconnect(u64),
}

pub struct SocketHandle {
    pub path: PathBuf,
}

impl SocketHandle {
    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// UID of the peer of `stream` (`SO_PEERCRED`).
pub fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd, correctly sized out-parameter.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(cred.uid)
}

fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Bind `<dir>/<pid>.sock` (dir 0700, socket 0600) and start serving.
pub fn start(dir: &Path, tx: async_channel::Sender<Incoming>) -> std::io::Result<SocketHandle> {
    start_with_uid(dir, tx, unsafe { libc::geteuid() })
}

/// Like [`start`] with an explicit allowed peer UID (tests).
pub fn start_with_uid(dir: &Path, tx: async_channel::Sender<Incoming>, allowed_uid: u32) -> std::io::Result<SocketHandle> {
    prepare_dir(dir)?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    if path.as_os_str().len() >= 104 {
        return Err(std::io::Error::other(format!("socket path too long for sun_path: {}", path.display())));
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new().name("gtk-mcp-accept".into()).spawn(move || accept_loop(listener.incoming(), tx, allowed_uid))?;
    Ok(SocketHandle { path })
}

/// Pause after a failed accept(): EMFILE/ENFILE leave the connection queued,
/// so retrying at once would spin until a descriptor frees up.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

fn accept_loop(
    incoming: impl Iterator<Item = std::io::Result<UnixStream>>,
    tx: async_channel::Sender<Incoming>,
    allowed_uid: u32,
) {
    let next_conn = AtomicU64::new(1);
    for stream in incoming {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(ACCEPT_BACKOFF);
                continue;
            }
        };
        match peer_uid(&stream) {
            Ok(uid) if uid == allowed_uid => {}
            _ => continue, // dropped: wrong or unknown peer
        }
        let id = next_conn.fetch_add(1, Ordering::Relaxed);
        serve(id, stream, tx.clone());
    }
}

fn serve(conn_id: u64, stream: UnixStream, tx: async_channel::Sender<Incoming>) {
    let Ok(write_half) = stream.try_clone() else { return };
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    let inflight: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
    let conn = Conn { id: conn_id, tx: out_tx, inflight: inflight.clone(), closed: Arc::default() };

    let _ = std::thread::Builder::new().name("gtk-mcp-write".into()).spawn(move || {
        let mut w = write_half;
        while let Ok(buf) = out_rx.recv() {
            if w.write_all(&buf).is_err() {
                break;
            }
        }
        let _ = w.shutdown(std::net::Shutdown::Both);
    });

    let _ = std::thread::Builder::new().name("gtk-mcp-read".into()).spawn(move || {
        let mut reader = FrameReader::new(BufReader::new(stream));
        loop {
            let frame = match reader.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) | Err(_) => break,
            };
            match frame {
                Frame::Oversized { len } => conn.send_line(&Response::err(
                    None,
                    AgentError::invalid_params(format!("frame of {len} bytes exceeds the maximum frame size")),
                )),
                Frame::Line(line) => match parse_message(&line) {
                    Err((id, e)) => conn.send_line(&Response::err(id, AgentError::invalid_params(e.to_string()))),
                    Ok(Message::Notification(n)) if n.method == method::CANCEL => {
                        if let Some(id) = n.params.get("id").and_then(Value::as_u64) {
                            if let Some(flag) = inflight.lock().unwrap().get(&id) {
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                    }
                    Ok(Message::Request(r)) if r.method == method::CANCEL => {
                        if let Some(id) = r.params.get("id").and_then(Value::as_u64) {
                            if let Some(flag) = inflight.lock().unwrap().get(&id) {
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                        conn.respond(r.id, Ok(json!({})));
                    }
                    Ok(Message::Request(r)) => {
                        let received = Instant::now();
                        let deadline = r.meta().map(|m| received + Duration::from_millis(m.deadline_ms));
                        let cancelled = Arc::new(AtomicBool::new(false));
                        inflight.lock().unwrap().insert(r.id, cancelled.clone());
                        let req = Req {
                            conn: conn.clone(),
                            id: r.id,
                            method: r.method,
                            params: r.params,
                            received,
                            deadline,
                            cancelled,
                        };
                        if tx.send_blocking(Incoming::Request(req)).is_err() {
                            break;
                        }
                    }
                    Ok(Message::Notification(_)) | Ok(Message::Response(_)) => {}
                },
            }
        }
        // Mark the connection closed before `Disconnect` is queued, so state
        // registered for it afterwards (the log hub) sees it is gone.
        conn.closed.store(true, Ordering::SeqCst);
        for flag in inflight.lock().unwrap().values() {
            flag.store(true, Ordering::SeqCst);
        }
        let _ = tx.send_blocking(Incoming::Disconnect(conn_id));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read};
    use std::os::unix::net::UnixStream;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gtkmcp-sock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn permissions_and_peer_refusal_and_bad_frames() {
        let dir = tmpdir("a");
        let (tx, rx) = async_channel::unbounded();
        let h = start(&dir, tx).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&h.path).unwrap().permissions().mode() & 0o777, 0o600);

        let mut s = UnixStream::connect(&h.path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        // malformed frame -> invalid_params, connection survives
        s.write_all(b"{not json\n").unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["data"]["code"], "invalid_params");
        // a valid request still arrives afterwards, with a deadline
        s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"snapshot\",\"params\":{\"_meta\":{\"deadline_ms\":50}}}\n").unwrap();
        match rx.recv_blocking().unwrap() {
            Incoming::Request(q) => {
                assert_eq!((q.id, q.method.as_str()), (3, "snapshot"));
                assert!(q.deadline.is_some());
                // cancel from the socket thread flips the flag without the main loop
                s.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"$/cancel\",\"params\":{\"id\":3}}\n").unwrap();
                for _ in 0..100 {
                    if q.cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert!(q.cancelled.load(Ordering::SeqCst));
                q.conn.respond(3, Ok(json!({"ok": true})));
            }
            _ => panic!("expected request"),
        }
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"));
        drop(s);
        drop(r);
        match rx.recv_blocking().unwrap() {
            Incoming::Disconnect(_) => {}
            _ => panic!("expected disconnect"),
        }
        h.cleanup();
        assert!(!h.path.exists());
    }

    #[test]
    fn oversized_frame_is_rejected_without_closing() {
        let dir = tmpdir("b");
        let (tx, _rx) = async_channel::unbounded();
        let h = start(&dir, tx).unwrap();
        let mut s = UnixStream::connect(&h.path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let big = vec![b'x'; gtk_mcp_protocol::MAX_FRAME_BYTES + 10];
        s.write_all(&big).unwrap();
        s.write_all(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$/cancel\",\"params\":{}}\n").unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["data"]["code"], "invalid_params");
        line.clear();
        r.read_line(&mut line).unwrap(); // the connection still answers the next frame
        assert!(line.contains("\"id\":1"));
        h.cleanup();
    }

    #[test]
    fn other_uid_is_refused() {
        let dir = tmpdir("c");
        let (tx, rx) = async_channel::unbounded();
        let me = unsafe { libc::geteuid() };
        let h = start_with_uid(&dir, tx, me.wrapping_add(1)).unwrap();
        assert_eq!(peer_uid(&UnixStream::connect(&h.path).unwrap()).unwrap(), me);
        let mut s = UnixStream::connect(&h.path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{}}\n");
        // refused: the server closes the connection without answering or enqueueing
        let mut buf = [0u8; 16];
        assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
        assert!(rx.try_recv().is_err());
        h.cleanup();
    }

    /// gm-n4j: a result nested deeper than the broker can parse is answered
    /// with an error carrying the request id, never sent as an unparsable frame.
    #[test]
    fn too_deeply_nested_result_becomes_an_error_for_its_id() {
        let (conn, rx) = Conn::open(1);
        let deep = (0..gtk_mcp_protocol::MAX_FRAME_NESTING).fold(json!(1), |v, _| json!([v]));
        conn.respond(9, Ok(deep));
        let line = rx.try_recv().expect("an answer was sent");
        let v: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["id"], 9);
        assert_eq!(v["error"]["data"]["code"], "invalid_value");
        assert!(v["error"]["message"].as_str().unwrap().contains("levels deep"), "{v}");
    }

    /// gm-7yw: failing accept() calls (EMFILE leaves the connection queued)
    /// are retried after a pause, never in a tight loop.
    #[test]
    fn accept_errors_back_off() {
        let (tx, rx) = async_channel::unbounded();
        let errors = std::iter::repeat_with(|| Err(std::io::Error::from_raw_os_error(libc::EMFILE))).take(4);
        let t = Instant::now();
        accept_loop(errors, tx, 0);
        assert!(t.elapsed() >= ACCEPT_BACKOFF * 4, "{:?}", t.elapsed());
        assert!(rx.try_recv().is_err());
    }
}
