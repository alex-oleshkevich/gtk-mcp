//! Diagnostics state shared by `watch`, `record` and the agent loop: the
//! reentrancy flag that marks agent-originated emissions, the event sink, and
//! the per-connection ownership of subscriptions and recordings (plan 5.6, 8).

use crate::socket::Conn;
use gtk_mcp_protocol::method;
use serde_json::{Value, json};
use std::cell::Cell;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

thread_local! {
    /// Depth of agent request code currently being polled on this thread.
    static BUSY: Cell<u32> = const { Cell::new(0) };
}

/// Is the agent itself running code right now? Emissions and notifications
/// seen while this holds were caused by the agent, not by the user or the app.
pub fn agent_busy() -> bool {
    BUSY.with(|b| b.get() > 0)
}

/// Marks every poll of the wrapped request future as agent activity. The flag
/// is clear while the future is suspended, so user input handled by the main
/// loop between polls is never attributed to the agent.
pub struct Busy<F>(pub F);

impl<F: Future> Future for Busy<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: structural pinning of the only field.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        BUSY.with(|b| b.set(b.get() + 1));
        let r = inner.poll(cx);
        BUSY.with(|b| b.set(b.get() - 1));
        r
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Longest string value carried in an event.
pub const MAX_EVENT_TEXT: usize = 512;

/// Shorten long strings in an event value.
pub fn clip(v: Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_EVENT_TEXT => {
            json!({"text": s.chars().take(MAX_EVENT_TEXT).collect::<String>(), "truncated": true})
        }
        other => other,
    }
}

/// Send one `$/event` to the subscriber's connection.
pub fn emit_event(conn: &Conn, mut event: Value) {
    if let Some(o) = event.as_object_mut() {
        o.insert("time_ms".into(), json!(now_ms()));
        o.insert("by_agent".into(), json!(agent_busy()));
    }
    conn.notify(method::EVENT, event);
}
