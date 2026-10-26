//! `inspect_shortcuts` and `activate_shortcut` (tools.md actions).
//!
//! `activate_shortcut` compares a keyval trigger (accelerator syntax) with
//! the `GtkKeyvalTrigger`s of shortcut controllers and runs the matching
//! `GtkShortcutAction` with `gtk_shortcut_action_activate`. No key event is
//! synthesized: a key press would need the compositor's focus and keyboard
//! state (`wayland-mcp`). Non-keyval triggers (mnemonics, `never`) are listed
//! but never matched.

use super::observe;
use crate::actions::active_window;
use crate::agent::{Agent, Ctx};
use crate::actions::variant_to_json;
use crate::json::object_json;
use crate::{selector, widgets};
use gio::prelude::*;
use glib::translate::IntoGlib;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{ActivateShortcutParams, AgentError, ErrorCode, InspectShortcutsParams};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::rc::Rc;

pub fn enum_nick<T: StaticType + ToValue>(v: &T) -> String {
    let val = v.to_value();
    glib::EnumValue::from_value(&val).map_or_else(|| "unknown".into(), |(_, e)| e.nick().to_owned())
}

pub fn flags_nicks<T: StaticType + ToValue>(v: &T) -> Vec<String> {
    let val = v.to_value();
    glib::FlagsValue::from_value(&val).map_or_else(Vec::new, |(_, fs)| fs.iter().map(|f| f.nick().to_owned()).collect())
}

pub fn controllers_of(w: &gtk::Widget) -> Vec<gtk::EventController> {
    let model = w.observe_controllers();
    (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gtk::EventController>()).collect()
}

fn shortcut_controllers(w: &gtk::Widget) -> Vec<gtk::ShortcutController> {
    controllers_of(w).into_iter().filter_map(|c| c.downcast::<gtk::ShortcutController>().ok()).collect()
}

fn tree(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    let mut c = w.first_child();
    while let Some(child) = c {
        tree(&child, out);
        c = child.next_sibling();
    }
}

fn trigger_json(t: &gtk::ShortcutTrigger) -> Value {
    let mut m = Map::new();
    m.insert("text".into(), json!(t.to_str().to_string()));
    if let Some(k) = t.downcast_ref::<gtk::KeyvalTrigger>() {
        m.insert("kind".into(), json!("keyval"));
        m.insert("keyval".into(), json!(k.keyval().into_glib()));
        m.insert("key_name".into(), json!(k.keyval().name().map(|n| n.to_string())));
        m.insert("modifiers".into(), json!(flags_nicks(&k.modifiers())));
    } else if let Some(k) = t.downcast_ref::<gtk::MnemonicTrigger>() {
        m.insert("kind".into(), json!("mnemonic"));
        m.insert("keyval".into(), json!(k.keyval().into_glib()));
    } else if let Some(a) = t.downcast_ref::<gtk::AlternativeTrigger>() {
        m.insert("kind".into(), json!("alternative"));
        m.insert("first".into(), trigger_json(&a.first()));
        m.insert("second".into(), trigger_json(&a.second()));
    } else if t.is::<gtk::NeverTrigger>() {
        m.insert("kind".into(), json!("never"));
    } else {
        m.insert("kind".into(), json!(t.type_().name()));
    }
    Value::Object(m)
}

fn action_json(a: &gtk::ShortcutAction) -> Value {
    let mut m = Map::new();
    m.insert("text".into(), json!(a.to_str().to_string()));
    if let Some(n) = a.downcast_ref::<gtk::NamedAction>() {
        m.insert("kind".into(), json!("named"));
        m.insert("action_name".into(), json!(n.action_name().to_string()));
    } else if a.is::<gtk::CallbackAction>() {
        m.insert("kind".into(), json!("callback"));
    } else if let Some(s) = a.downcast_ref::<gtk::SignalAction>() {
        m.insert("kind".into(), json!("signal"));
        m.insert("signal_name".into(), json!(s.signal_name().to_string()));
    } else if a.is::<gtk::MnemonicAction>() {
        m.insert("kind".into(), json!("mnemonic"));
    } else if a.is::<gtk::ActivateAction>() {
        m.insert("kind".into(), json!("activate"));
    } else if a.is::<gtk::NothingAction>() {
        m.insert("kind".into(), json!("nothing"));
    } else {
        m.insert("kind".into(), json!(a.type_().name()));
    }
    Value::Object(m)
}

fn shortcuts_of(c: &gtk::ShortcutController) -> Vec<gtk::Shortcut> {
    (0..c.n_items()).filter_map(|i| c.item(i).and_downcast::<gtk::Shortcut>()).collect()
}

fn widget_brief(agent: &Agent, w: &gtk::Widget) -> Value {
    let mut m = Map::new();
    m.insert("ref".into(), json!(agent.registry.ref_for(w)));
    m.insert("type".into(), json!(widgets::type_name(w)));
    if let Some(a) = widgets::automation_id(w) {
        m.insert("automation_id".into(), json!(a));
    }
    Value::Object(m)
}

fn controller_json(agent: &Agent, w: &gtk::Widget, c: &gtk::ShortcutController) -> Value {
    let shortcuts: Vec<Value> = shortcuts_of(c)
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut m = Map::new();
            m.insert("index".into(), json!(i));
            m.insert("trigger".into(), s.trigger().map_or(Value::Null, |t| trigger_json(&t)));
            m.insert("action".into(), s.action().map_or(Value::Null, |a| action_json(&a)));
            if let Some(args) = s.arguments() {
                m.insert("arguments".into(), variant_to_json(&args));
            }
            Value::Object(m)
        })
        .collect();
    json!({
        "controller": object_json(agent, Some(w.upcast_ref()), c.upcast_ref()),
        "widget": widget_brief(agent, w),
        "scope": enum_nick(&c.scope()),
        "propagation_phase": enum_nick(&c.propagation_phase()),
        "mnemonics_modifiers": flags_nicks(&c.mnemonics_modifiers()),
        "name": c.name().map(|n| n.to_string()),
        "shortcuts": shortcuts,
    })
}

/// `[{action, accels}]` of the application's accelerators.
fn app_accels(agent: &Agent) -> Vec<(String, Vec<String>)> {
    let mut v: Vec<(String, Vec<String>)> = agent
        .app
        .list_action_descriptions()
        .iter()
        .map(|d| (d.to_string(), agent.app.accels_for_action(d).iter().map(|a| a.to_string()).collect::<Vec<_>>()))
        .filter(|(_, a)| !a.is_empty())
        .collect();
    v.sort();
    v
}

pub fn inspect_shortcuts(agent: &Rc<Agent>, p: InspectShortcutsParams) -> Result<Value, AgentError> {
    let mut widgets_all = Vec::new();
    match &p.target {
        Some(t) => tree(&selector::resolve_widget(agent, t, false)?, &mut widgets_all),
        None => {
            for w in selector::toplevels() {
                tree(w.upcast_ref(), &mut widgets_all);
            }
        }
    }
    let mut controllers = Vec::new();
    for w in &widgets_all {
        for c in shortcut_controllers(w) {
            controllers.push(controller_json(agent, w, &c));
        }
    }
    let mut m = Map::new();
    m.insert("total".into(), json!(controllers.len()));
    m.insert("controllers".into(), Value::Array(controllers));
    if p.target.is_none() {
        m.insert(
            "app_accels".into(),
            Value::Array(app_accels(agent).into_iter().map(|(a, k)| json!({"action": a, "accels": k})).collect()),
        );
    }
    Ok(Value::Object(m))
}

// -- activate_shortcut --------------------------------------------------------------

fn norm_mods(m: gdk::ModifierType) -> gdk::ModifierType {
    m & gtk::accelerator_get_default_mod_mask()
}

/// Keyval/modifier pairs a trigger fires on (alternatives flattened).
fn keyval_pairs(t: &gtk::ShortcutTrigger, out: &mut Vec<(gdk::Key, gdk::ModifierType)>) {
    if let Some(k) = t.downcast_ref::<gtk::KeyvalTrigger>() {
        out.push((k.keyval(), k.modifiers()));
    } else if let Some(a) = t.downcast_ref::<gtk::AlternativeTrigger>() {
        keyval_pairs(&a.first(), out);
        keyval_pairs(&a.second(), out);
    }
}

fn trigger_matches(t: &gtk::ShortcutTrigger, key: gdk::Key, mods: gdk::ModifierType) -> bool {
    let mut pairs = Vec::new();
    keyval_pairs(t, &mut pairs);
    pairs.iter().any(|(k, m)| k.to_lower() == key.to_lower() && norm_mods(*m) == norm_mods(mods))
}

pub async fn activate_shortcut(agent: &Rc<Agent>, p: ActivateShortcutParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let (key, mods) = gtk::accelerator_parse(p.trigger.as_str()).filter(|(k, _)| k.into_glib() != 0).ok_or_else(|| {
        AgentError::new(
            ErrorCode::InvalidValue,
            "the trigger is not a keyval accelerator (expected e.g. '<Control>s', '<Shift><Alt>F4')",
        )
    })?;
    let normalized = gtk::accelerator_name(key, mods).to_string();

    let start: gtk::Widget = match &p.target {
        Some(t) => selector::resolve_widget(agent, t, false)?,
        None => {
            let win = active_window(agent).ok_or_else(|| AgentError::new(ErrorCode::NotFound, "the application has no window"))?;
            gtk::prelude::GtkWindowExt::focus(&win).unwrap_or_else(|| win.upcast())
        }
    };

    // Candidates: the focus chain (all scopes), then managed/global controllers of the whole window.
    let mut chain = Vec::new();
    let mut cur = Some(start.clone());
    while let Some(w) = cur {
        cur = w.parent();
        chain.push(w);
    }
    let mut seen: HashSet<usize> = HashSet::new();
    let mut cands: Vec<(gtk::Widget, gtk::ShortcutController)> = Vec::new();
    for w in &chain {
        for c in shortcut_controllers(w) {
            if seen.insert(c.as_ptr() as usize) {
                cands.push((w.clone(), c));
            }
        }
    }
    if let Some(top) = chain.last() {
        let mut all = Vec::new();
        tree(top, &mut all);
        for w in &all {
            for c in shortcut_controllers(w) {
                if c.scope() != gtk::ShortcutScope::Local && seen.insert(c.as_ptr() as usize) {
                    cands.push((w.clone(), c));
                }
            }
        }
    }

    let mut matched: Vec<Value> = Vec::new();
    let mut activated_by: Option<Value> = None;
    for (w, c) in &cands {
        for (i, s) in shortcuts_of(c).iter().enumerate() {
            let Some(t) = s.trigger() else { continue };
            if !trigger_matches(&t, key, mods) {
                continue;
            }
            let Some(action) = s.action() else { continue };
            ctx.check()?;
            let handled = action.activate(gtk::ShortcutActionFlags::EXCLUSIVE, w, s.arguments().as_ref());
            let rec = json!({
                "source": "shortcut_controller",
                "controller": object_json(agent, Some(w.upcast_ref()), c.upcast_ref()),
                "widget": widget_brief(agent, w),
                "scope": enum_nick(&c.scope()),
                "shortcut_index": i,
                "trigger": trigger_json(&t),
                "action": action_json(&action),
                "handled": handled,
            });
            if handled && activated_by.is_none() {
                activated_by = Some(rec.clone());
            }
            matched.push(rec);
            if handled {
                break;
            }
        }
        if activated_by.is_some() {
            break;
        }
    }

    // The application's accelerators (`gtk_application_set_accels_for_action`).
    if activated_by.is_none() {
        'accels: for (desc, accels) in app_accels(agent) {
            for accel in accels {
                let Some((k, m)) = gtk::accelerator_parse(accel.as_str()) else { continue };
                if k.to_lower() != key.to_lower() || norm_mods(m) != norm_mods(mods) {
                    continue;
                }
                let (name, target) = gio::Action::parse_detailed_name(&desc)
                    .map_err(|_| AgentError::new(ErrorCode::Unavailable, format!("the application accelerator action '{desc}' is not parseable")))?;
                ctx.check()?;
                let handled = start.activate_action(&name, target.as_ref()).is_ok();
                let rec = json!({"source": "app_accel", "action": desc, "accel": accel, "handled": handled});
                if handled {
                    activated_by = Some(rec.clone());
                }
                matched.push(rec);
                if handled {
                    break 'accels;
                }
            }
        }
    }

    if matched.is_empty() {
        return Err(AgentError::new(
            ErrorCode::NotFound,
            format!("no shortcut matches '{normalized}' from {} (searched {} shortcut controllers and the application accelerators)",
                agent.registry.ref_for(&start), cands.len()),
        )
        .with_data(json!({"trigger": normalized, "controllers_searched": cands.len()})));
    }
    let observed = json!({"activated": activated_by.is_some()});
    let mut res = observe::finish(agent, agent.registry.ref_for(&start), observed, Some(&start), p.observe.as_ref(), ctx).await;
    res["trigger"] = json!(normalized);
    res["activated"] = json!(activated_by.is_some());
    res["matched"] = activated_by.unwrap_or_else(|| matched[0].clone());
    res["candidates"] = Value::Array(matched);
    Ok(res)
}
