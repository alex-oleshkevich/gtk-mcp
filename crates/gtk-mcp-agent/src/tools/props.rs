//! `list_properties`, `get_property`, `set_property` (tools.md core).
//! Targets are any object: a widget ref / selector or an `o<N>` ref. Values
//! go through `crate::json`, which enforces secrecy (plan 4.4).

use crate::agent::{Agent, Ctx};
use crate::json::{json_to_value, numeric_range, scalar_to_json, value_to_json};
use crate::{secrecy, selector};
use glib::prelude::*;
use gtk4 as gtk;
use gtk_mcp_protocol::{AgentError, ErrorCode, GetPropertyParams, ListPropertiesParams, Observe, SetPropertyParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;

fn find_pspec(obj: &glib::Object, name: &str) -> Result<glib::ParamSpec, AgentError> {
    obj.find_property(name).ok_or_else(|| {
        AgentError::invalid_params(format!("type {} has no property '{name}'", obj.type_().name()))
    })
}

fn describe_pspec(pspec: &glib::ParamSpec) -> Value {
    let f = pspec.flags();
    let mut m = Map::new();
    m.insert("name".into(), json!(pspec.name()));
    m.insert("type".into(), json!(pspec.value_type().name()));
    m.insert("owner".into(), json!(pspec.owner_type().name()));
    m.insert("readable".into(), json!(f.contains(glib::ParamFlags::READABLE)));
    m.insert("writable".into(), json!(f.contains(glib::ParamFlags::WRITABLE)));
    m.insert("construct_only".into(), json!(f.contains(glib::ParamFlags::CONSTRUCT_ONLY)));
    if f.contains(glib::ParamFlags::DEPRECATED) {
        m.insert("deprecated".into(), json!(true));
    }
    if let Some(b) = pspec.blurb().filter(|b| !b.is_empty()) {
        m.insert("blurb".into(), json!(b));
    }
    if let Some(d) = scalar_to_json(pspec, pspec.default_value()).filter(|d| !d.is_null()) {
        m.insert("default".into(), d);
    }
    if let Some((min, max)) = numeric_range(pspec) {
        m.insert("minimum".into(), min);
        m.insert("maximum".into(), max);
    }
    let vt = pspec.value_type();
    if vt.is_a(glib::Type::ENUM) {
        if let Some(c) = glib::EnumClass::with_type(vt) {
            let vals: Vec<Value> = c.values().iter().map(|e| json!({"nick": e.nick(), "value": e.value()})).collect();
            m.insert("enum_values".into(), Value::Array(vals));
        }
    } else if vt.is_a(glib::Type::FLAGS) {
        if let Some(c) = glib::FlagsClass::with_type(vt) {
            let vals: Vec<Value> = c.values().iter().map(|e| json!({"nick": e.nick(), "value": e.value()})).collect();
            m.insert("enum_values".into(), Value::Array(vals));
        }
    }
    Value::Object(m)
}

pub fn list_properties(agent: &Rc<Agent>, p: ListPropertiesParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    let mut props = Vec::new();
    for ps in obj.list_properties() {
        if p.owner.as_deref().is_some_and(|o| ps.owner_type().name() != o) {
            continue;
        }
        if p.name.as_deref().is_some_and(|n| ps.name() != n && ps.name().replace('-', "_") != n.replace('-', "_")) {
            continue;
        }
        props.push(describe_pspec(&ps));
    }
    if props.is_empty() {
        if let Some(n) = &p.name {
            if obj.find_property(n).is_none() {
                return Err(AgentError::invalid_params(format!("type {} has no property '{n}'", obj.type_().name())));
            }
        }
    }
    Ok(json!({"target": agent.registry.ref_for(&obj), "total": props.len(), "properties": props}))
}

pub fn get_property(agent: &Rc<Agent>, p: GetPropertyParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    if p.names.is_empty() {
        return Err(AgentError::invalid_params("names must list at least one property"));
    }
    let mut values = Map::new();
    for name in &p.names {
        let ps = find_pspec(&obj, name)?;
        if !ps.flags().contains(glib::ParamFlags::READABLE) {
            return Err(AgentError::new(
                ErrorCode::UnsupportedOperation,
                format!("property '{name}' of {} is not readable", ps.owner_type().name()),
            ));
        }
        let v = obj.property_value(ps.name());
        values.insert(name.clone(), value_to_json(agent, &obj, &ps, &v));
    }
    Ok(json!({"target": agent.registry.ref_for(&obj), "values": values}))
}

/// Adapter to the `observe` module. The parent replaces the body by
/// `crate::tools::observe::observe(agent, w, obs, ctx).await`.
async fn observe_hook(
    agent: &Rc<Agent>,
    w: &gtk::Widget,
    obs: &Observe,
    ctx: &Ctx,
) -> (Option<Value>, Option<AgentError>) {
    let _ = (agent, w, obs, ctx);
    (None, Some(AgentError::new(ErrorCode::UnsupportedOperation, "observe not available")))
}

pub async fn set_property(agent: &Rc<Agent>, p: SetPropertyParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    let ps = find_pspec(&obj, &p.name)?;
    let val = json_to_value(agent, &ps, &p.value)?;
    obj.set_property_from_value(ps.name(), &val);

    let observed = if ps.flags().contains(glib::ParamFlags::READABLE) {
        let v = obj.property_value(ps.name());
        value_to_json(agent, &obj, &ps, &v)
    } else if secrecy::is_secret(&obj) {
        secrecy::redacted()
    } else {
        Value::Null
    };
    let mut m = Map::new();
    m.insert("target".into(), json!(agent.registry.ref_for(&obj)));
    m.insert("name".into(), json!(ps.name()));
    m.insert("observed".into(), observed);

    if let Some(obs) = &p.observe {
        let (observation, error) = match obj.downcast_ref::<gtk::Widget>() {
            Some(w) => observe_hook(agent, w, obs, ctx).await,
            None => (
                None,
                Some(AgentError::new(ErrorCode::UnsupportedOperation, "observe needs a widget target")),
            ),
        };
        if let Some(o) = observation {
            m.insert("observation".into(), o);
        }
        if let Some(e) = error {
            m.insert("observation_error".into(), serde_json::to_value(e).unwrap_or(Value::Null));
        }
    }
    Ok(Value::Object(m))
}
