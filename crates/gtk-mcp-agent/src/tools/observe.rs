//! `observe`: the post-action observation attached to mutating tools
//! (plan 5.2, tools.md "Mutating tools accept `observe`"). Dispatch has
//! already succeeded when this runs, so a failed observation is reported next
//! to the mutation result and never retried.

use super::capture::capture_widget;
use super::snapshot::{self, Limits};
use crate::agent::{Agent, Ctx};
use gtk4 as gtk;
use gtk_mcp_protocol::{AgentError, DEFAULT_SETTLE_TIMEOUT_MS, Observe, SnapshotView};
use serde_json::{Map, Value, json};
use std::rc::Rc;

/// Run the requested observations of `w`.
///
/// Returns `(parts, error)`: `parts` is an object holding only the
/// observations that succeeded (`snapshot`, `capture`), `None` when nothing was
/// requested or nothing succeeded; `error` is the first failure.
pub async fn observe(agent: &Rc<Agent>, w: &gtk::Widget, obs: &Observe, ctx: &Ctx) -> (Option<Value>, Option<AgentError>) {
    let mut parts = Map::new();
    let mut error = None;

    if let Some(depth) = obs.snapshot_depth {
        let limits = Limits {
            max_depth: depth as usize,
            max_nodes: snapshot::DEFAULT_MAX_NODES,
            include_unmapped: true,
            include_closed_popovers: false,
            text_limit: snapshot::DEFAULT_TEXT_LIMIT,
        };
        parts.insert("snapshot".into(), snapshot::build(agent, std::slice::from_ref(w), &limits, &SnapshotView::Full));
    }
    if obs.capture == Some(true) {
        match capture_widget(agent, w, true, DEFAULT_SETTLE_TIMEOUT_MS, false, 1.0, ctx).await {
            Ok(resp) => {
                parts.insert("capture".into(), serde_json::to_value(resp).unwrap_or_else(|e| json!({"error": e.to_string()})));
            }
            Err(e) => error = Some(e),
        }
    }
    (if parts.is_empty() { None } else { Some(Value::Object(parts)) }, error)
}

/// Serialized `InteractResult` shape shared by `interact`, `edit_text`
/// and `set_property`: `{target, observed, observation?, observation_error?}`.
pub async fn finish(
    agent: &Rc<Agent>,
    target_ref: String,
    observed: Value,
    w: Option<&gtk::Widget>,
    obs: Option<&Observe>,
    ctx: &Ctx,
) -> Value {
    let mut m = Map::new();
    m.insert("target".into(), json!(target_ref));
    m.insert("observed".into(), observed);
    match (obs, w) {
        (Some(obs), Some(w)) => {
            let (observation, error) = observe(agent, w, obs, ctx).await;
            if let Some(o) = observation {
                m.insert("observation".into(), o);
            }
            if let Some(e) = error {
                m.insert("observation_error".into(), serde_json::to_value(e).expect("AgentError serializes"));
            }
        }
        (Some(obs), None) if obs.snapshot_depth.is_some() || obs.capture == Some(true) => {
            let e = AgentError::new(
                gtk_mcp_protocol::ErrorCode::UnsupportedOperation,
                "observe needs a widget target; the target is not a widget",
            );
            m.insert("observation_error".into(), serde_json::to_value(e).expect("AgentError serializes"));
        }
        _ => {}
    }
    Value::Object(m)
}
