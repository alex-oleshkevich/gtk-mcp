//! `list_actions`, `inspect_action`, `activate_action`, `change_action_state`
//! (tools.md actions). Actions are addressed as `{scope, name}`; see
//! `crate::actions` for scope resolution and what GTK lets us enumerate.

use super::observe;
use crate::actions::{
    self, Addr, ClassAction, GroupRef, Lookup, Scope, ScopeKind, describe_group_action, describe_group_entry, lookup, parameter_for,
    parameter_untyped, resolve_addr, resolve_scope, variant_to_json,
};
use crate::agent::{Agent, Ctx};
use crate::secrecy;
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{
    ActivateActionParams, AgentError, ChangeActionStateParams, ErrorCode, InspectActionParams, ListActionsParams,
};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::rc::Rc;

const INCOMPLETE_REASON: &str = "groups inserted with insert_action_group are not enumerable through public GTK API; \
     only registered groups (register_action_group), the application, the application window and widget-class \
     actions are listed. Unregistered groups still activate by name";

fn not_found(msg: String) -> AgentError {
    AgentError::new(ErrorCode::NotFound, msg)
}

fn scope_json(agent: &Agent, scope: &Scope) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), json!(scope.kind.as_str()));
    if let Some(w) = &scope.widget {
        m.insert("ref".into(), json!(agent.registry.ref_for(w)));
        m.insert("type".into(), json!(w.type_().name()));
    }
    Value::Object(m)
}

fn push_group(agent: &Agent, out: &mut Vec<Value>, g: &GroupRef, secret: bool) {
    let mut names: Vec<String> = g.group.list_actions().iter().map(|s| s.to_string()).collect();
    names.sort();
    for n in names {
        out.push(describe_group_entry(agent, g, &n, secret));
    }
}

pub fn list_actions(agent: &Rc<Agent>, p: ListActionsParams) -> Result<Value, AgentError> {
    let scope = resolve_scope(agent, p.scope.as_ref(), None)?;
    let secret = scope.is_secret();
    let mut entries: Vec<Value> = Vec::new();

    // Enumerable groups reachable from the scope, nearest first.
    let mut groups: Vec<GroupRef> = Vec::new();
    let mut class_sources: Vec<gtk::Widget> = Vec::new();
    match scope.kind {
        ScopeKind::App => groups.push(actions::app_group(agent)),
        ScopeKind::Window | ScopeKind::Widget => {
            let chain = if scope.kind == ScopeKind::Window { scope.widget.iter().cloned().collect() } else { scope.chain() };
            let mut win_done = false;
            for w in &chain {
                if !win_done && w.is::<gtk::ApplicationWindow>() {
                    if let Some(g) = actions::find_group(agent, &Scope { kind: ScopeKind::Widget, widget: Some(w.clone()) }, "win")? {
                        groups.push(g);
                        win_done = true;
                    }
                }
                for g in actions::registered_at(w) {
                    groups.push(g);
                }
                class_sources.push(w.clone());
            }
            if scope.kind == ScopeKind::Widget {
                groups.push(actions::app_group(agent));
            }
        }
    }
    for g in &groups {
        push_group(agent, &mut entries, g, secret);
    }
    for w in &class_sources {
        let mut cls: Vec<ClassAction> = actions::class_actions(w);
        cls.sort_by(|a, b| a.name.cmp(&b.name));
        for c in cls {
            entries.push(c.describe(agent, w));
        }
    }

    if let Some(pre) = p.prefix.as_deref().map(|s| s.trim_end_matches('.')) {
        entries.retain(|e| e["name"].as_str().is_some_and(|n| n.split_once('.').is_some_and(|(a, _)| a == pre)));
    }
    // A nearer definition shadows farther ones of the same name.
    let mut seen = HashSet::new();
    for e in &mut entries {
        let name = e["name"].as_str().unwrap_or_default().to_owned();
        if !seen.insert(name) {
            e["shadowed"] = json!(true);
        }
    }
    let complete = scope.kind == ScopeKind::App;
    let mut m = Map::new();
    m.insert("scope".into(), scope_json(agent, &scope));
    m.insert("complete".into(), json!(complete));
    if !complete {
        m.insert("incomplete_reason".into(), json!(INCOMPLETE_REASON));
    }
    m.insert("total".into(), json!(entries.len()));
    m.insert("actions".into(), Value::Array(entries));
    Ok(Value::Object(m))
}

pub(crate) fn describe_lookup(agent: &Agent, l: &Lookup, secret: bool) -> Option<Value> {
    match l {
        Lookup::Group(g, action) => Some(describe_group_entry(agent, g, action, secret)),
        Lookup::Class(w, c) => Some(c.describe(agent, w)),
        Lookup::Missing | Lookup::Unresolved => None,
    }
}

fn unreachable_error(addr: &Addr, l: &Lookup) -> AgentError {
    let full = addr.full_name();
    match l {
        Lookup::Missing => not_found(format!("no action '{full}' in the {} scope", addr.scope.kind.as_str())),
        _ => not_found(format!(
            "no enumerable action '{full}': the '{}.' group is not registered (register_action_group), so its actions \
             cannot be inspected; activate_action can still try it by name",
            addr.prefix
        )),
    }
}

pub fn inspect_action(agent: &Rc<Agent>, p: InspectActionParams) -> Result<Value, AgentError> {
    let addr = resolve_addr(agent, p.scope.as_ref(), &p.name)?;
    let l = lookup(agent, &addr)?;
    let secret = addr.scope.is_secret();
    let mut v = describe_lookup(agent, &l, secret).ok_or_else(|| unreachable_error(&addr, &l))?;
    v["scope"] = scope_json(agent, &addr.scope);
    Ok(v)
}

pub async fn activate_action(agent: &Rc<Agent>, p: ActivateActionParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let addr = resolve_addr(agent, p.scope.as_ref(), &p.name)?;
    let l = lookup(agent, &addr)?;
    let secret = addr.scope.is_secret();
    let full = addr.full_name();
    let widget = addr.scope.widget.clone();
    let mut extra = Map::new();
    extra.insert("action".into(), json!(full));
    let observed;
    match &l {
        Lookup::Group(g, action) => {
            if !g.group.is_action_enabled(action) {
                return Err(AgentError::new(ErrorCode::UnsupportedOperation, format!("action '{full}' is disabled")));
            }
            let param = parameter_for(g.group.action_parameter_type(action).as_deref(), p.parameter.as_ref())?;
            if let Some(s) = g.group.action_state(action) {
                if !secret {
                    extra.insert("state_before".into(), variant_to_json(&s));
                }
            }
            g.group.activate_action(action, param.as_ref());
            // The handler may have changed what the action is; read it fresh.
            observed = if g.group.has_action(action) {
                describe_group_action(&g.group, &g.prefix, action, secret)
            } else {
                json!({"name": full, "removed": true})
            };
        }
        Lookup::Class(w, c) => {
            let ptype = c.parameter_type.as_deref().map(glib::VariantTy::new).transpose().ok().flatten();
            let param = parameter_for(ptype, p.parameter.as_ref())?;
            w.activate_action(&full, param.as_ref())
                .map_err(|_| AgentError::new(ErrorCode::UnsupportedOperation, format!("action '{full}' is disabled or cannot run now")))?;
            observed = c.describe(agent, w);
        }
        Lookup::Missing => return Err(unreachable_error(&addr, &l)),
        Lookup::Unresolved => {
            let w = widget.clone().ok_or_else(|| unreachable_error(&addr, &l))?;
            let param = parameter_untyped(p.parameter.as_ref())?;
            w.activate_action(&full, param.as_ref()).map_err(|_| {
                AgentError::new(
                    ErrorCode::NotFound,
                    format!("action '{full}' was not found from {} (or it is disabled)", agent.registry.ref_for(&w)),
                )
            })?;
            observed = json!({"name": full, "state_observable": false});
        }
    }
    ctx.check()?;
    extra.insert("activated".into(), json!(true));
    let mut res = observe::finish(agent, addr.scope.target_ref(agent), observed, widget.as_ref(), p.observe.as_ref(), ctx).await;
    for (k, v) in extra {
        res[k] = v;
    }
    Ok(res)
}

pub async fn change_action_state(agent: &Rc<Agent>, p: ChangeActionStateParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let addr = resolve_addr(agent, p.scope.as_ref(), &p.name)?;
    let l = lookup(agent, &addr)?;
    let secret = addr.scope.is_secret();
    let full = addr.full_name();
    let (g, action) = match &l {
        Lookup::Group(g, a) => (g.clone(), a.clone()),
        Lookup::Class(..) => {
            return Err(AgentError::new(
                ErrorCode::UnsupportedOperation,
                format!("'{full}' is a widget-class action; its state is not accessible through public API"),
            ));
        }
        Lookup::Unresolved => {
            return Err(AgentError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "the '{}.' group is not registered (register_action_group): its state type is unknown and no group \
                     handle is reachable, so the state cannot be changed or observed",
                    addr.prefix
                ),
            ));
        }
        Lookup::Missing => return Err(unreachable_error(&addr, &l)),
    };
    let Some(state_type) = g.group.action_state_type(&action) else {
        return Err(AgentError::new(ErrorCode::UnsupportedOperation, format!("action '{full}' is stateless")));
    };
    if !g.group.is_action_enabled(&action) {
        return Err(AgentError::new(ErrorCode::UnsupportedOperation, format!("action '{full}' is disabled")));
    }
    let requested = actions::json_to_variant(&state_type, &p.value).map_err(|m| AgentError::new(ErrorCode::InvalidValue, m))?;
    let before = g.group.action_state(&action);
    g.group.change_action_state(&action, &requested);
    ctx.check()?;
    let after = g.group.action_state(&action);
    let accepted = after.as_ref() == Some(&requested);
    let observed = if g.group.has_action(&action) {
        describe_group_action(&g.group, &g.prefix, &action, secret)
    } else {
        json!({"name": full, "removed": true})
    };
    let widget = addr.scope.widget.clone();
    let mut res = observe::finish(agent, addr.scope.target_ref(agent), observed, widget.as_ref(), p.observe.as_ref(), ctx).await;
    res["action"] = json!(full);
    res["accepted"] = json!(accepted);
    if secret {
        res["requested"] = secrecy::redacted();
    } else {
        res["requested"] = variant_to_json(&requested);
        if let Some(b) = before {
            res["state_before"] = variant_to_json(&b);
        }
    }
    Ok(res)
}
