//! Tool handlers. Each runs on the GTK main thread inside the executor.

use crate::agent::{Agent, Ctx};
use gtk_mcp_protocol::AgentError;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::rc::Rc;

pub mod app;
pub mod batch;
pub mod capture;
pub mod inspect;
pub mod interact;
pub mod model;
pub mod observe;
pub mod props;
pub mod query;
pub mod signals;
pub mod snapshot;
pub mod text;
pub mod wait;
pub mod windows;

pub fn parse<T: DeserializeOwned>(tool: &str, params: Value) -> Result<T, AgentError> {
    serde_json::from_value(params).map_err(|e| AgentError::invalid_params(format!("invalid params for {tool}: {e}")))
}

pub async fn dispatch(agent: &Rc<Agent>, tool: &str, params: Value, ctx: &Ctx) -> Result<Value, AgentError> {
    match tool {
        "batch" => batch::batch(agent, parse(tool, params)?, ctx).await,
        "list_properties" => props::list_properties(agent, parse(tool, params)?),
        "get_property" => props::get_property(agent, parse(tool, params)?),
        "set_property" => props::set_property(agent, parse(tool, params)?, ctx).await,
        "list_signals" => signals::list_signals(agent, parse(tool, params)?),
        "inspect_model" => model::inspect_model(agent, parse(tool, params)?),
        "wait_for" => wait::wait_for(agent, parse(tool, params)?, ctx).await,
        "interact" => interact::interact(agent, parse(tool, params)?, ctx).await,
        "inspect_text" => text::inspect_text(agent, parse(tool, params)?),
        "edit_text" => text::edit_text(agent, parse(tool, params)?, ctx).await,
        "capture" => capture::capture(agent, parse(tool, params)?, ctx).await,
        "list_windows" => windows::list_windows(agent),
        "snapshot" => snapshot::snapshot(agent, parse(tool, params)?),
        "query" => query::query(agent, parse(tool, params)?),
        "inspect" => inspect::inspect(agent, parse(tool, params)?),
        "app_open" => app::app_open(agent, parse(tool, params)?),
        "inspect_gsettings" => app::inspect_gsettings(agent, parse(tool, params)?),
        "set_gsetting" => app::set_gsetting(agent, parse(tool, params)?),
        "list_resources" => app::list_resources(parse(tool, params)?),
        "read_resource" => app::read_resource(parse(tool, params)?),
        "object_stats" => app::object_stats(agent, parse(tool, params)?),
        other => Err(AgentError::new(
            gtk_mcp_protocol::ErrorCode::NotFound,
            format!("method '{other}' is not implemented by this agent"),
        )),
    }
}
