//! `list_signals`: signal schemas of an object's type, its ancestors and
//! interfaces, with watch eligibility (plan 5.5: `notify` through
//! `connect_notify`, everything else through emission hooks; signals flagged
//! `G_SIGNAL_NO_HOOKS` cannot be hooked).

use crate::agent::Agent;
use crate::selector;
use glib::prelude::*;
use glib::translate::*;
use gtk_mcp_protocol::{AgentError, ListSignalsParams};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::rc::Rc;

/// Signal ids declared directly by `t` (not inherited).
fn declared_signals(t: glib::Type) -> Vec<glib::subclass::SignalId> {
    let mut n = 0u32;
    // SAFETY: returns a g_malloc'ed array of `n` ids (NULL when n == 0), freed below.
    unsafe {
        let ids = glib::gobject_ffi::g_signal_list_ids(t.into_glib(), &mut n);
        if ids.is_null() {
            return Vec::new();
        }
        let out = (0..n as usize).map(|i| from_glib(*ids.add(i))).collect();
        glib::ffi::g_free(ids as *mut _);
        out
    }
}

const FLAG_NAMES: &[(glib::SignalFlags, &str)] = &[
    (glib::SignalFlags::RUN_FIRST, "run_first"),
    (glib::SignalFlags::RUN_LAST, "run_last"),
    (glib::SignalFlags::RUN_CLEANUP, "run_cleanup"),
    (glib::SignalFlags::NO_RECURSE, "no_recurse"),
    (glib::SignalFlags::DETAILED, "detailed"),
    (glib::SignalFlags::ACTION, "action"),
    (glib::SignalFlags::NO_HOOKS, "no_hooks"),
    (glib::SignalFlags::MUST_COLLECT, "must_collect"),
    (glib::SignalFlags::DEPRECATED, "deprecated"),
];

fn watch_info(name: &str, flags: glib::SignalFlags) -> Value {
    if name == "notify" {
        json!({"eligible": true, "mechanism": "notify"})
    } else if flags.contains(glib::SignalFlags::NO_HOOKS) {
        json!({"eligible": false, "mechanism": null, "reason": "signal is flagged G_SIGNAL_NO_HOOKS: emission hooks cannot observe it"})
    } else {
        json!({"eligible": true, "mechanism": "emission_hook"})
    }
}

pub fn list_signals(agent: &Rc<Agent>, p: ListSignalsParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    // Most derived class first, then its parents, then every implemented interface.
    let mut types = Vec::new();
    let mut t = Some(obj.type_());
    while let Some(ty) = t {
        types.push(ty);
        t = ty.parent();
    }
    types.extend(obj.type_().interfaces().iter().copied());

    let mut seen = HashSet::new();
    let mut signals = Vec::new();
    for ty in types {
        for id in declared_signals(ty) {
            if !seen.insert(id.into_glib()) {
                continue;
            }
            let q = id.query();
            let name = q.signal_name();
            let owner = q.type_().name();
            if p.owner.as_deref().is_some_and(|o| o != owner) || p.name.as_deref().is_some_and(|n| n != name) {
                continue;
            }
            let flags = q.flags();
            let flag_list: Vec<&str> = FLAG_NAMES.iter().filter(|(f, _)| flags.contains(*f)).map(|(_, n)| *n).collect();
            let params: Vec<&str> = q.param_types().iter().map(|t| t.type_().name()).collect();
            signals.push(json!({
                "name": name,
                "owner": owner,
                "return_type": q.return_type().type_().name(),
                "param_types": params,
                "flags": flag_list,
                "detailed": flags.contains(glib::SignalFlags::DETAILED),
                "watch": watch_info(name, flags),
            }));
        }
    }
    if signals.is_empty() {
        if let Some(n) = &p.name {
            if glib::subclass::SignalId::lookup(n, obj.type_()).is_none() {
                return Err(AgentError::invalid_params(format!("type {} has no signal '{n}'", obj.type_().name())));
            }
        }
    }
    Ok(json!({"target": agent.registry.ref_for(&obj), "signals": signals}))
}
