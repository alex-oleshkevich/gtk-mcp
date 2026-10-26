//! `inspect_controllers` (tools.md actions): event controllers and gestures
//! of a widget, through `gtk_widget_observe_controllers` and public getters.

use super::shortcuts::{controllers_of, enum_nick, flags_nicks};
use crate::agent::Agent;
use crate::json::{object_json, scalar_to_json};
use crate::selector;
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, InspectControllersParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;

fn controller_json(agent: &Agent, w: &gtk::Widget, c: &gtk::EventController) -> Value {
    let mut m = Map::new();
    let mut o = object_json(agent, Some(w.upcast_ref()), c.upcast_ref());
    m.extend(o.as_object_mut().map(std::mem::take).unwrap_or_default());
    m.insert("name".into(), json!(c.name().map(|n| n.to_string())));
    m.insert("propagation_phase".into(), json!(enum_nick(&c.propagation_phase())));
    m.insert("propagation_limit".into(), json!(enum_nick(&c.propagation_limit())));

    if let Some(g) = c.downcast_ref::<gtk::Gesture>() {
        let mut gm = Map::new();
        gm.insert("n_points".into(), g.property::<u32>("n-points").into());
        if let Some(s) = g.downcast_ref::<gtk::GestureSingle>() {
            gm.insert("button".into(), json!(s.button()));
            gm.insert("touch_only".into(), json!(s.is_touch_only()));
            gm.insert("exclusive".into(), json!(s.is_exclusive()));
        }
        m.insert("gesture".into(), Value::Object(gm));
    }

    // Everything the concrete controller classes declare below GtkEventController.
    let base = gtk::EventController::static_type();
    let mut config = Map::new();
    for ps in c.list_properties() {
        let owner = ps.owner_type();
        if owner == base || !owner.is_a(base) || !ps.flags().contains(glib::ParamFlags::READABLE) {
            continue;
        }
        if let Some(v) = scalar_to_json(&ps, &c.property_value(ps.name())) {
            config.insert(ps.name().to_owned(), v);
        }
    }
    if !config.is_empty() {
        m.insert("config".into(), Value::Object(config));
    }
    if let Some(s) = c.downcast_ref::<gtk::ShortcutController>() {
        m.insert("scope".into(), json!(enum_nick(&s.scope())));
        m.insert("n_shortcuts".into(), json!(s.n_items()));
        m.insert("mnemonics_modifiers".into(), json!(flags_nicks(&s.mnemonics_modifiers())));
    }
    Value::Object(m)
}

pub fn inspect_controllers(agent: &Rc<Agent>, p: InspectControllersParams) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, false)?;
    let controllers: Vec<Value> = controllers_of(&w).iter().map(|c| controller_json(agent, &w, c)).collect();
    Ok(json!({"target": agent.registry.ref_for(&w), "total": controllers.len(), "controllers": controllers}))
}
