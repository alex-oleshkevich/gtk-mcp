//! Tool handlers. Each runs on the GTK main thread inside the executor.

use crate::agent::{Agent, Ctx};
use gtk_mcp_protocol::AgentError;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::rc::Rc;

pub mod actions;
pub mod batch;
pub mod capture;
pub mod layout;
pub mod inspect;
pub mod interact;
pub mod render;
pub mod settings;
pub mod style;
pub mod controllers;
pub mod menu;
pub mod model;
pub mod observe;
pub mod props;
pub mod shortcuts;
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
        // layout-style (M3)
        "inspect_layout" => layout::inspect_layout(agent, parse(tool, params)?),
        "audit_layout" => layout::audit_layout(agent, parse(tool, params)?),
        "inspect_style" => style::inspect_style(agent, parse(tool, params)?),
        "apply_css" => style::apply_css(agent, parse(tool, params)?, ctx).await,
        "remove_css" => style::remove_css(parse(tool, params)?, ctx).await,
        "highlight" => style::highlight(agent, parse(tool, params)?, ctx).await,
        "inspect_render" => render::inspect_render(agent, parse(tool, params)?, ctx).await,
        "inspect_settings" => settings::inspect_settings(agent, parse(tool, params)?),
        "override_setting" => settings::override_setting(agent, parse(tool, params)?, ctx),
        "list_actions" => actions::list_actions(agent, parse(tool, params)?),
        "inspect_action" => actions::inspect_action(agent, parse(tool, params)?),
        "activate_action" => actions::activate_action(agent, parse(tool, params)?, ctx).await,
        "change_action_state" => actions::change_action_state(agent, parse(tool, params)?, ctx).await,
        "inspect_action_binding" => menu::inspect_action_binding(agent, parse(tool, params)?),
        "inspect_menu" => menu::inspect_menu(agent, parse(tool, params)?),
        "inspect_shortcuts" => shortcuts::inspect_shortcuts(agent, parse(tool, params)?),
        "activate_shortcut" => shortcuts::activate_shortcut(agent, parse(tool, params)?, ctx).await,
        "inspect_controllers" => controllers::inspect_controllers(agent, parse(tool, params)?),
        other => Err(AgentError::new(
            gtk_mcp_protocol::ErrorCode::NotFound,
            format!("method '{other}' is not implemented by this agent"),
        )),
    }
}
