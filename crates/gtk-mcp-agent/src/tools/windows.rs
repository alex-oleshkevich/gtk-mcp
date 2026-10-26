//! `list_windows`: toplevels and mapped native surfaces (popovers).

use crate::agent::Agent;
use crate::{layer, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::AgentError;
use serde_json::{Value, json};
use std::rc::Rc;

fn renderer_name(w: &impl IsA<gtk::Native>) -> Option<String> {
    w.renderer().map(|r| r.type_().name().to_string())
}

fn window_json(agent: &Agent, win: &gtk::Window) -> Value {
    let reg = &agent.registry;
    let mut m = serde_json::Map::new();
    m.insert("ref".into(), json!(reg.ref_for(win)));
    m.insert("type".into(), json!(widgets::type_name(win)));
    m.insert("title".into(), json!(win.title().map(|t| t.to_string())));
    m.insert("visible".into(), json!(win.is_visible()));
    m.insert("mapped".into(), json!(win.is_mapped()));
    m.insert("active".into(), json!(win.is_active()));
    m.insert("width".into(), json!(win.width()));
    m.insert("height".into(), json!(win.height()));
    m.insert("renderer".into(), json!(renderer_name(win)));
    m.insert("surface_scale".into(), json!(win.scale_factor()));
    m.insert("monitor".into(), json!(selector::monitor_connector(win)));
    m.insert("modal".into(), json!(win.is_modal()));
    m.insert("fullscreen".into(), json!(win.is_fullscreen()));
    m.insert("maximized".into(), json!(win.is_maximized()));
    m.insert("transient_for".into(), json!(win.transient_for().map(|t| reg.ref_for(&t))));
    m.insert("default_widget".into(), json!(win.default_widget().map(|d| reg.ref_for(&d))));
    if let Some(aw) = win.downcast_ref::<gtk::ApplicationWindow>() {
        m.insert("app_window_id".into(), json!(aw.id()));
    }
    if let Some(l) = layer::config(win) {
        m.insert("layer_shell".into(), l);
    }
    Value::Object(m)
}

fn collect_natives(agent: &Agent, w: &gtk::Widget, out: &mut Vec<Value>) {
    for c in widgets::children(w) {
        let cw = c.widget;
        if cw.is_mapped() && cw.is::<gtk::Popover>() {
            out.push(json!({
                "ref": agent.registry.ref_for(&cw),
                "type": widgets::type_name(&cw),
                "kind": "popover",
                "parent": agent.registry.ref_for(w),
                "width": cw.width(),
                "height": cw.height(),
                "renderer": cw.downcast_ref::<gtk::Native>().and_then(renderer_name),
            }));
        }
        collect_natives(agent, &cw, out);
    }
}

pub fn list_windows(agent: &Rc<Agent>) -> Result<Value, AgentError> {
    let wins = selector::toplevels();
    let mut natives = Vec::new();
    for w in &wins {
        collect_natives(agent, w.upcast_ref(), &mut natives);
    }
    Ok(json!({
        "windows": wins.iter().map(|w| window_json(agent, w)).collect::<Vec<_>>(),
        "natives": natives,
    }))
}
