//! `forward_log`: records from the app's own GLib log writer reach the
//! broker's `read_logs` (`docs/embedding.md` "Logs", plan 8). The agent never
//! installs a writer: GLib allows one per process and libraries must not take
//! it. The app calls [`forward_log`] from its writer; the call may come from any
//! thread, so the connections live behind a mutex and nothing here touches GTK.

use crate::socket::Conn;
use glib::{LogField, LogLevel};
use gtk_mcp_protocol::method;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Records kept while no broker is connected.
const BACKLOG: usize = 512;
/// Longest field value forwarded, in bytes.
const MAX_FIELD_BYTES: usize = 4096;
const MAX_FIELDS: usize = 32;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static USED: AtomicBool = AtomicBool::new(false);

struct Hub {
    conns: Vec<Conn>,
    backlog: VecDeque<Value>,
}

static HUB: Mutex<Hub> = Mutex::new(Hub { conns: Vec::new(), backlog: VecDeque::new() });

fn hub() -> MutexGuard<'static, Hub> {
    HUB.lock().unwrap_or_else(|e| e.into_inner())
}

/// The agent is attached and active: `forward_log` forwards.
pub fn activate() {
    ACTIVE.store(true, Ordering::SeqCst);
}

/// Has the app ever called `forward_log`? Advertised as `LogForwarding`.
pub fn used() -> bool {
    USED.load(Ordering::SeqCst)
}

/// A broker connected: it receives everything forwarded while none was.
pub fn register_conn(conn: &Conn) {
    let mut h = hub();
    if h.conns.iter().any(|c| c.id == conn.id) {
        return;
    }
    h.conns.push(conn.clone());
    for rec in h.backlog.drain(..) {
        conn.notify(method::LOG, rec);
    }
}

pub fn unregister_conn(conn: u64) {
    hub().conns.retain(|c| c.id != conn);
}

fn severity(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Critical => "critical",
        LogLevel::Warning => "warning",
        LogLevel::Message => "message",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

fn clip(s: &str) -> (String, bool) {
    if s.len() <= MAX_FIELD_BYTES {
        return (s.to_owned(), false);
    }
    let mut end = MAX_FIELD_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_owned(), true)
}

/// Forward one record from the app's own log writer to `read_logs`.
///
/// Call it from the function passed to `glib::log_set_writer_func`, with the
/// writer's own arguments. Thread-safe; a no-op unless the agent is active.
/// String fields are forwarded as text, binary fields are skipped.
pub fn forward_log(level: LogLevel, fields: &[LogField<'_>]) {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    USED.store(true, Ordering::SeqCst);
    let mut map = Map::new();
    let mut message = None;
    let mut domain = None;
    for f in fields.iter().take(MAX_FIELDS) {
        let Some(v) = f.value_str() else { continue };
        let (text, _) = clip(v);
        match f.key() {
            "MESSAGE" => message = Some(text),
            "GLIB_DOMAIN" => domain = Some(text),
            k => {
                map.insert(k.to_owned(), Value::String(text));
            }
        }
    }
    let time_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let rec = json!({
        "source": "forwarded",
        "level": severity(level),
        "domain": domain,
        "message": message,
        "fields": map,
        "time_ms": time_ms,
    });
    let mut h = hub();
    if h.conns.is_empty() {
        if h.backlog.len() == BACKLOG {
            h.backlog.pop_front();
        }
        h.backlog.push_back(rec);
    } else {
        for c in &h.conns {
            c.notify(method::LOG, rec.clone());
        }
    }
}
