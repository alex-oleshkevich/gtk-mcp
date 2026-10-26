//! `watch` / `unwatch`: subscriptions that push `$/event` notifications into
//! the broker's ring buffer (`read_events`). Three kinds:
//!
//! * `notify`: `connect_notify_local` on the target (`GObject::notify` cannot
//!   be hooked);
//! * `signal`: a shared emission hook filtered to the target instance;
//! * `model`: `items-changed` and, for selection models, `selection-changed`.
//!
//! Events of secret objects carry property/signal names only (plan 4.4).
//! Subscriptions belong to the connection that made them and are released on
//! disconnect.

use crate::agent::{Agent, Ctx};
use crate::diag::{clip, emit_event};
use crate::hooks::{self, ListenerId};
use crate::json::{object_json, object_of, scalar_to_json, value_to_json};
use crate::socket::Conn;
use crate::{secrecy, selector};
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, UnwatchParams, WatchKind, WatchParams};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const MAX_SUBSCRIPTIONS: usize = 256;
const MAX_NOTIFY_PROPERTIES: usize = 64;

enum Cleanup {
    Handler(glib::WeakRef<glib::Object>, glib::SignalHandlerId),
    Hook(ListenerId),
}

struct Sub {
    id: String,
    conn: u64,
    cleanup: Vec<Cleanup>,
}

thread_local! {
    static SUBS: RefCell<Vec<Sub>> = const { RefCell::new(Vec::new()) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

fn release(sub: Sub) {
    for c in sub.cleanup {
        match c {
            Cleanup::Handler(weak, id) => {
                if let Some(o) = weak.upgrade() {
                    o.disconnect(id);
                }
            }
            Cleanup::Hook(l) => hooks::remove(l),
        }
    }
}

/// A connection dropped: its subscriptions go with it.
pub fn release_conn(conn: u64) {
    let gone: Vec<Sub> = SUBS.with(|s| {
        let mut s = s.borrow_mut();
        let (gone, keep) = std::mem::take(&mut *s).into_iter().partition(|x| x.conn == conn);
        *s = keep;
        gone
    });
    gone.into_iter().for_each(release);
}

pub fn release_all() {
    let gone = SUBS.with(|s| std::mem::take(&mut *s.borrow_mut()));
    gone.into_iter().for_each(release);
}

/// JSON of a signal parameter. Strings and objects of a secret instance are
/// never included by the caller; objects are refs (with inherited secrecy).
fn param_json(agent: &Agent, owner: &glib::Object, v: &glib::Value) -> Value {
    if let Some(o) = object_of(v) {
        return object_json(agent, Some(owner), &o);
    }
    let dummy = glib::ParamSpecBoolean::builder("x").build();
    scalar_to_json(&dummy, v).unwrap_or_else(|| crate::json::unsupported_type(v.type_()))
}

fn notify_handler(conn: Conn, sub: String) -> impl Fn(&glib::Object, &glib::ParamSpec) + 'static {
    move |obj, pspec| {
        let Some(agent) = Agent::current() else { return };
        let mut ev = Map::new();
        ev.insert("kind".into(), json!("notify"));
        ev.insert("subscription".into(), json!(sub));
        ev.insert("target".into(), json!(agent.registry.ref_for(obj)));
        ev.insert("type".into(), json!(obj.type_().name()));
        ev.insert("property".into(), json!(pspec.name()));
        if secrecy::is_secret(obj) {
            ev.insert("secret".into(), json!(true));
        } else if pspec.flags().contains(glib::ParamFlags::READABLE) {
            let v = obj.property_value(pspec.name());
            ev.insert("value".into(), clip(value_to_json(&agent, obj, pspec, &v)));
        }
        emit_event(&conn, Value::Object(ev));
    }
}

fn watch_notify(
    agent: &Rc<Agent>,
    obj: &glib::Object,
    properties: &Option<Vec<String>>,
    conn: &Conn,
    id: &str,
    out: &mut Vec<Cleanup>,
) -> Result<Value, AgentError> {
    let names: Vec<Option<String>> = match properties {
        None => vec![None],
        Some(p) if p.is_empty() => return Err(AgentError::invalid_params("properties must name at least one property (omit it to watch all)")),
        Some(p) if p.len() > MAX_NOTIFY_PROPERTIES => {
            return Err(AgentError::invalid_params(format!("at most {MAX_NOTIFY_PROPERTIES} properties per subscription")));
        }
        Some(p) => {
            let mut v = Vec::new();
            for n in p {
                let ps = obj.find_property(n).ok_or_else(|| {
                    AgentError::invalid_params(format!("type {} has no property '{n}'", obj.type_().name()))
                })?;
                v.push(Some(ps.name().to_string()));
            }
            v
        }
    };
    let _ = agent;
    let mut shown = Vec::new();
    for n in names {
        let h = obj.connect_notify_local(n.as_deref(), notify_handler(conn.clone(), id.to_owned()));
        out.push(Cleanup::Handler(obj.downgrade(), h));
        shown.push(n);
    }
    Ok(json!({"properties": shown.iter().map(|n| n.as_deref().map_or(json!("*"), |s| json!(s))).collect::<Vec<_>>()}))
}

fn watch_signal(
    obj: &glib::Object,
    signal: &str,
    conn: &Conn,
    id: &str,
    out: &mut Vec<Cleanup>,
) -> Result<Value, AgentError> {
    if signal == "notify" {
        return Err(AgentError::invalid_params("use kind `notify` to watch property notifications"));
    }
    let Some(sid) = glib::subclass::SignalId::lookup(signal, obj.type_()) else {
        return Err(AgentError::invalid_params(format!("type {} has no signal '{signal}'", obj.type_().name())));
    };
    if sid.query().flags().contains(glib::SignalFlags::NO_HOOKS) {
        return Err(AgentError::new(
            ErrorCode::UnsupportedOperation,
            format!("signal '{signal}' is flagged G_SIGNAL_NO_HOOKS: emission hooks cannot observe it"),
        ));
    }
    let target = obj.downgrade();
    let conn = conn.clone();
    let sub = id.to_owned();
    let name = signal.to_owned();
    let listener: hooks::Listener = Rc::new(move |instance, values| {
        if target.upgrade().as_ref() != Some(instance) {
            return;
        }
        let Some(agent) = Agent::current() else { return };
        let mut ev = Map::new();
        ev.insert("kind".into(), json!("signal"));
        ev.insert("subscription".into(), json!(sub));
        ev.insert("target".into(), json!(agent.registry.ref_for(instance)));
        ev.insert("type".into(), json!(instance.type_().name()));
        ev.insert("signal".into(), json!(name));
        if secrecy::is_secret(instance) {
            ev.insert("secret".into(), json!(true));
        } else {
            let args: Vec<Value> = values.iter().skip(1).map(|v| clip(param_json(&agent, instance, v))).collect();
            ev.insert("args".into(), Value::Array(args));
        }
        emit_event(&conn, Value::Object(ev));
    });
    let l = hooks::add(obj.type_(), signal, listener)
        .ok_or_else(|| AgentError::new(ErrorCode::UnsupportedOperation, format!("cannot hook signal '{signal}'")))?;
    out.push(Cleanup::Hook(l));
    Ok(json!({"signal": signal}))
}

fn watch_model(
    agent: &Rc<Agent>,
    obj: &glib::Object,
    conn: &Conn,
    id: &str,
    out: &mut Vec<Cleanup>,
) -> Result<Value, AgentError> {
    let model = crate::tools::model::resolve_model(obj)?;
    let mobj: glib::Object = model.clone().upcast();
    {
        let (conn, sub) = (conn.clone(), id.to_owned());
        let h = model.connect_items_changed(move |m, position, removed, added| {
            let Some(agent) = Agent::current() else { return };
            emit_event(
                &conn,
                json!({
                    "kind": "items_changed", "subscription": sub,
                    "target": agent.registry.ref_for(m),
                    "position": position, "removed": removed, "added": added, "n_items": m.n_items(),
                }),
            );
        });
        out.push(Cleanup::Handler(mobj.downgrade(), h));
    }
    let mut selection = false;
    if let Some(sel) = model.dynamic_cast_ref::<gtk::SelectionModel>() {
        selection = true;
        let (conn, sub) = (conn.clone(), id.to_owned());
        let h = sel.connect_selection_changed(move |m, position, n_items| {
            let Some(agent) = Agent::current() else { return };
            emit_event(
                &conn,
                json!({
                    "kind": "selection_changed", "subscription": sub,
                    "target": agent.registry.ref_for(m),
                    "position": position, "n_items": n_items,
                }),
            );
        });
        out.push(Cleanup::Handler(mobj.downgrade(), h));
    }
    Ok(json!({"model": agent.registry.ref_for(&mobj), "selection_changed": selection}))
}

pub fn watch(agent: &Rc<Agent>, p: WatchParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    if SUBS.with(|s| s.borrow().len()) >= MAX_SUBSCRIPTIONS {
        return Err(AgentError::invalid_params(format!("at most {MAX_SUBSCRIPTIONS} subscriptions; unwatch some first")));
    }
    let id = NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        format!("sub{v}")
    });
    let mut cleanup = Vec::new();
    let detail = match &p.watch {
        WatchKind::Notify { properties } => watch_notify(agent, &obj, properties, &ctx.conn, &id, &mut cleanup),
        WatchKind::Signal { signal } => watch_signal(&obj, signal, &ctx.conn, &id, &mut cleanup),
        WatchKind::Model => watch_model(agent, &obj, &ctx.conn, &id, &mut cleanup),
    };
    let detail = match detail {
        Ok(d) => d,
        Err(e) => {
            release(Sub { id, conn: ctx.conn.id, cleanup });
            return Err(e);
        }
    };
    SUBS.with(|s| s.borrow_mut().push(Sub { id: id.clone(), conn: ctx.conn.id, cleanup }));
    let mut m = Map::new();
    m.insert("subscription".into(), json!(id));
    m.insert("target".into(), json!(agent.registry.ref_for(&obj)));
    m.insert("kind".into(), json!(p.watch.name()));
    if secrecy::is_secret(&obj) {
        m.insert("secret".into(), json!(true));
        m.insert("note".into(), json!("events of secret objects carry names only, never values"));
    }
    if let Value::Object(d) = detail {
        m.extend(d);
    }
    Ok(Value::Object(m))
}

pub fn unwatch(p: UnwatchParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let sub = SUBS.with(|s| {
        let mut s = s.borrow_mut();
        let i = s.iter().position(|x| x.id == p.subscription && x.conn == ctx.conn.id)?;
        Some(s.remove(i))
    });
    match sub {
        Some(sub) => {
            release(sub);
            Ok(json!({"subscription": p.subscription, "removed": true}))
        }
        None => Err(AgentError::new(
            ErrorCode::NotFound,
            format!("no subscription '{}' on this connection", p.subscription),
        )),
    }
}
