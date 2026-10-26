//! Tool handlers. Each runs on the GTK main thread inside the executor.

use crate::agent::{Agent, Ctx};
use gtk_mcp_protocol::AgentError;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::rc::Rc;

pub mod inspect;
pub mod query;
pub mod snapshot;
pub mod windows;

pub fn parse<T: DeserializeOwned>(tool: &str, params: Value) -> Result<T, AgentError> {
    serde_json::from_value(params).map_err(|e| AgentError::invalid_params(format!("invalid params for {tool}: {e}")))
}

pub async fn dispatch(agent: &Rc<Agent>, tool: &str, params: Value, ctx: &Ctx) -> Result<Value, AgentError> {
    let _ = ctx;
    match tool {
        "list_windows" => windows::list_windows(agent),
        "snapshot" => snapshot::snapshot(agent, parse(tool, params)?),
        "query" => query::query(agent, parse(tool, params)?),
        "inspect" => inspect::inspect(agent, parse(tool, params)?),
        other => Err(AgentError::new(
            gtk_mcp_protocol::ErrorCode::NotFound,
            format!("method '{other}' is not implemented by this agent"),
        )),
    }
}
