//! `query`: resolve a selector to all matching refs.

use crate::agent::Agent;
use crate::selector;
use gtk_mcp_protocol::{AgentError, QueryParams};
use serde_json::{Value, json};
use std::rc::Rc;

pub const DEFAULT_LIMIT: usize = 100;

pub fn query(agent: &Rc<Agent>, p: QueryParams) -> Result<Value, AgentError> {
    let hits = selector::find(agent, &p.selector)?;
    let limit = p.limit.map(|l| l as usize).unwrap_or(DEFAULT_LIMIT);
    let total = hits.len();
    let matches: Vec<Value> = hits
        .iter()
        .take(limit)
        .map(|h| {
            let mut d = selector::describe(agent, &h.widget);
            if !h.attached {
                d["attached"] = json!(false);
            }
            d
        })
        .collect();
    let mut out = json!({"total": total, "truncated": total > matches.len(), "matches": matches});
    if total == 1 {
        out["target"] = out["matches"][0]["ref"].clone();
    }
    Ok(out)
}
