//! `inspect_action_binding` and `inspect_menu` (tools.md actions).

use super::actions::describe_lookup;
use crate::actions::{self, Addr, Lookup, Scope, ScopeKind, variant_to_json};
use crate::agent::Agent;
use crate::json::object_json;
use crate::{secrecy, selector};
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, InspectActionBindingParams, InspectMenuParams, Target};
use serde_json::{Map, Value, json};
use std::cell::Cell;
use std::rc::Rc;

fn widget_scope(w: &gtk::Widget) -> Scope {
    Scope { kind: if w.is::<gtk::Window>() { ScopeKind::Window } else { ScopeKind::Widget }, widget: Some(w.clone()) }
}

/// What `full_name` (`prefix.action`) resolves to from `scope`.
fn resolve_name(agent: &Agent, scope: &Scope, full_name: &str, secret: bool) -> Value {
    let Some((prefix, action)) = full_name.split_once('.').filter(|(p, a)| !p.is_empty() && !a.is_empty()) else {
        return json!({"found": false, "reason": "not a prefixed action name"});
    };
    let addr = Addr { scope: scope.clone(), prefix: prefix.to_owned(), action: action.to_owned() };
    match actions::lookup(agent, &addr) {
        Ok(l @ (Lookup::Group(..) | Lookup::Class(..))) => {
            let mut v = describe_lookup(agent, &l, secret).unwrap_or(Value::Null);
            v["found"] = json!(true);
            v
        }
        Ok(Lookup::Missing) => json!({"found": false, "reason": "the group has no such action"}),
        Ok(Lookup::Unresolved) => json!({
            "found": null,
            "reason": "the prefix is not enumerable from here (unregistered inserted group, or no such group); activation by name may still work"
        }),
        Err(e) => json!({"found": false, "reason": e.message}),
    }
}

pub fn inspect_action_binding(agent: &Rc<Agent>, p: InspectActionBindingParams) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, false)?;
    let secret = secrecy::is_secret(&w);
    let mut m = Map::new();
    m.insert("target".into(), json!(agent.registry.ref_for(&w)));
    m.insert("type".into(), json!(w.type_().name()));

    let (name, target, via): (Option<String>, Option<glib::Variant>, &str) = if let Some(a) = w.dynamic_cast_ref::<gtk::Actionable>() {
        (a.action_name().map(|s| s.to_string()), a.action_target_value(), "GtkActionable")
    } else if w.find_property("action-name").is_some_and(|ps| ps.value_type() == glib::Type::STRING) {
        let name = w.property::<Option<String>>("action-name");
        let target = w
            .find_property("action-target")
            .filter(|ps| ps.value_type() == glib::Type::VARIANT)
            .and_then(|_| w.property::<Option<glib::Variant>>("action-target"));
        (name, target, "properties")
    } else {
        (None, None, "none")
    };
    m.insert("actionable".into(), json!(via != "none"));
    m.insert("via".into(), json!(via));
    m.insert("action_name".into(), json!(name));
    match &target {
        Some(_) if secret => {
            m.insert("action_target".into(), secrecy::redacted());
        }
        Some(t) => {
            m.insert("action_target".into(), variant_to_json(t));
            m.insert("action_target_type".into(), json!(t.type_().as_str()));
        }
        None => {
            m.insert("action_target".into(), Value::Null);
        }
    }
    if let Some(n) = &name {
        if !secret {
            m.insert("detailed_name".into(), json!(gio::Action::print_detailed_name(n, target.as_ref()).to_string()));
        }
        let mut resolved = resolve_name(agent, &widget_scope(&w), n, secret);
        // Radio-style bindings: the action's state equals the binding's target.
        if let (Some(t), Some(st)) = (&target, resolved.get("state").cloned()) {
            if !secret && !st.is_null() {
                resolved["target_selected"] = json!(st == variant_to_json(t));
            }
        }
        m.insert("resolved".into(), resolved);
    }

    if let Some(ps) = w.find_property("menu-model") {
        if ps.value_type().is_a(gio::MenuModel::static_type()) {
            let mm = w.property::<Option<gio::MenuModel>>("menu-model");
            m.insert(
                "menu_model".into(),
                match mm {
                    Some(mm) => {
                        let mut o = object_json(agent, Some(w.upcast_ref()), mm.upcast_ref());
                        o["n_items"] = json!(mm.n_items());
                        o
                    }
                    None => Value::Null,
                },
            );
        }
    }
    if let Some(mb) = w.dynamic_cast_ref::<gtk::MenuButton>() {
        m.insert("popover".into(), mb.popover().map_or(Value::Null, |p| object_json(agent, Some(w.upcast_ref()), p.upcast_ref())));
    }
    Ok(Value::Object(m))
}

// -- inspect_menu ----------------------------------------------------------------

struct Walk<'a> {
    agent: &'a Agent,
    scope: Scope,
    secret: bool,
    max_depth: u32,
    budget: Cell<u32>,
    truncated: Cell<bool>,
}

impl Walk<'_> {
    fn menu(&self, model: &gio::MenuModel, depth: u32) -> Value {
        let n = model.n_items();
        if depth > self.max_depth {
            self.truncated.set(true);
            return json!({"n_items": n, "truncated": true});
        }
        let mut items = Vec::new();
        for i in 0..n {
            if self.budget.get() == 0 {
                self.truncated.set(true);
                break;
            }
            self.budget.set(self.budget.get() - 1);
            items.push(self.item(model, i, depth));
        }
        json!({"n_items": n, "items": items})
    }

    fn item(&self, model: &gio::MenuModel, i: i32, depth: u32) -> Value {
        let mut label = None;
        let mut action: Option<String> = None;
        let mut target: Option<glib::Variant> = None;
        let mut attrs = Map::new();
        let it = model.iterate_item_attributes(i);
        while let Some((name, value)) = it.next() {
            match name.as_str() {
                "label" => label = value.str().map(str::to_owned),
                "action" => action = value.str().map(str::to_owned),
                "target" => target = Some(value),
                other => {
                    attrs.insert(other.to_owned(), if self.secret { secrecy::redacted() } else { variant_to_json(&value) });
                }
            }
        }
        let mut m = Map::new();
        m.insert("index".into(), json!(i));
        let mut kind = "item";
        let mut links = Map::new();
        let lit = model.iterate_item_links(i);
        while let Some((name, sub)) = lit.next() {
            let nested = self.menu(&sub, depth + 1);
            match name.as_str() {
                "section" => {
                    kind = "section";
                    m.insert("section".into(), nested);
                }
                "submenu" => {
                    kind = "submenu";
                    m.insert("submenu".into(), nested);
                }
                other => {
                    links.insert(other.to_owned(), nested);
                }
            }
        }
        m.insert("kind".into(), json!(kind));
        m.insert("label".into(), if self.secret { label.as_ref().map_or(Value::Null, |_| secrecy::redacted()) } else { json!(label) });
        m.insert("action".into(), json!(action));
        match &target {
            Some(_) if self.secret => {
                m.insert("target".into(), secrecy::redacted());
            }
            Some(t) => {
                m.insert("target".into(), variant_to_json(t));
                m.insert("target_type".into(), json!(t.type_().as_str()));
            }
            None => {}
        }
        if let Some(a) = &action {
            if !self.secret {
                let detailed = gio::Action::print_detailed_name(a, target.as_ref()).to_string();
                let accels: Vec<String> = self.agent.app.accels_for_action(&detailed).iter().map(|s| s.to_string()).collect();
                if !accels.is_empty() {
                    m.insert("accels".into(), json!(accels));
                }
                m.insert("detailed_action".into(), json!(detailed));
            }
            let r = resolve_name(self.agent, &self.scope, a, self.secret);
            m.insert(
                "resolved".into(),
                json!({"found": r["found"], "enabled": r.get("enabled").cloned().unwrap_or(Value::Null)}),
            );
        }
        if !attrs.is_empty() {
            m.insert("attributes".into(), Value::Object(attrs));
        }
        if !links.is_empty() {
            m.insert("links".into(), Value::Object(links));
        }
        Value::Object(m)
    }
}

pub fn inspect_menu(agent: &Rc<Agent>, p: InspectMenuParams) -> Result<Value, AgentError> {
    let mut source = Map::new();
    let (model, scope, secret): (gio::MenuModel, Scope, bool) = match &p.target {
        Target::Ref(r) if r == "app" => {
            let mb = agent.app.menubar().ok_or_else(|| AgentError::new(ErrorCode::NotFound, "the application has no menubar"))?;
            source.insert("source".into(), json!("application menubar"));
            let scope = actions::resolve_scope(agent, Some(&Target::Ref("window".into())), None)
                .unwrap_or(Scope { kind: ScopeKind::App, widget: None });
            (mb, scope, false)
        }
        t => {
            let obj = selector::resolve_object(agent, t)?;
            if let Some(mm) = obj.dynamic_cast_ref::<gio::MenuModel>() {
                let scope = actions::resolve_scope(agent, Some(&Target::Ref("window".into())), None)
                    .unwrap_or(Scope { kind: ScopeKind::App, widget: None });
                source.insert("source".into(), json!("menu model"));
                (mm.clone(), scope, false)
            } else if let Some(w) = obj.dynamic_cast_ref::<gtk::Widget>() {
                let model_of = |w: &gtk::Widget| -> Option<gio::MenuModel> {
                    let ps = w.find_property("menu-model")?;
                    ps.value_type().is_a(gio::MenuModel::static_type()).then(|| w.property::<Option<gio::MenuModel>>("menu-model"))?
                };
                let mut from = w.clone();
                let mut model = model_of(w);
                if model.is_none() {
                    if let Some(pop) = w.dynamic_cast_ref::<gtk::MenuButton>().and_then(|mb| mb.popover()) {
                        model = model_of(pop.upcast_ref());
                        from = pop.upcast();
                    }
                }
                let model = model.ok_or_else(|| {
                    AgentError::new(
                        ErrorCode::UnsupportedOperation,
                        format!("{} has no menu model (property 'menu-model' unset or missing)", w.type_().name()),
                    )
                })?;
                source.insert("source".into(), json!({"widget": agent.registry.ref_for(&from), "property": "menu-model"}));
                (model, widget_scope(w), secrecy::is_secret(w))
            } else {
                return Err(AgentError::new(
                    ErrorCode::UnsupportedOperation,
                    format!("{} is neither a widget nor a GMenuModel", obj.type_().name()),
                ));
            }
        }
    };
    let walk = Walk {
        agent,
        scope,
        secret,
        max_depth: p.max_depth.unwrap_or(8),
        budget: Cell::new(p.max_items.unwrap_or(500)),
        truncated: Cell::new(false),
    };
    let tree = walk.menu(&model, 0);
    let mut m = source;
    m.insert("model".into(), object_json(agent, None, model.upcast_ref()));
    m.insert("menu".into(), tree);
    m.insert("truncated".into(), json!(walk.truncated.get()));
    Ok(Value::Object(m))
}
