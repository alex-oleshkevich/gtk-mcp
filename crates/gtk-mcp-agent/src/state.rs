//! App-provided debug state (`register_state`) and the GSettings objects the
//! app registered (`register_settings`). Both live on the main thread; the
//! broker reads them through the `state/list` and `state/read` methods and
//! serves them as `app-state/<name>` MCP resources (`docs/embedding.md`).

use crate::agent::Agent;
use gio::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, STATE_PREFIX, state_method};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

/// A state closure must not produce more than this many serialized bytes.
const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

type StateFn = Rc<dyn Fn() -> Value>;

#[derive(Default)]
pub struct AppState {
    states: RefCell<BTreeMap<String, StateFn>>,
    settings: RefCell<Vec<glib::WeakRef<gio::Settings>>>,
}

impl AppState {
    pub fn register(&self, name: String, read: StateFn) {
        let replaced = self.states.borrow_mut().insert(name, read);
        // Dropped after the borrow ends: dropping it may run app code (a C
        // caller's GDestroyNotify) that registers or reads state again.
        drop(replaced);
    }

    pub fn names(&self) -> Vec<String> {
        self.states.borrow().keys().cloned().collect()
    }

    pub fn read(&self, name: &str) -> Result<Value, AgentError> {
        // Clone the closure out so it may itself call `register_state`.
        let f = self
            .states
            .borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| AgentError::new(ErrorCode::NotFound, format!("no app state named '{name}'")))?;
        let value = catch_unwind(AssertUnwindSafe(|| f()))
            .map_err(|_| AgentError::new(ErrorCode::Unavailable, format!("the state closure '{name}' panicked")))?;
        let bytes = serde_json::to_vec(&value).map_or(usize::MAX, |b| b.len());
        if bytes > MAX_STATE_BYTES {
            return Err(AgentError::new(
                ErrorCode::Unavailable,
                format!("app state '{name}' exceeds {MAX_STATE_BYTES} serialized bytes"),
            ));
        }
        Ok(value)
    }

    pub fn add_settings(&self, s: &gio::Settings) {
        let mut v = self.settings.borrow_mut();
        v.retain(|w| w.upgrade().is_some());
        if !v.iter().any(|w| w.upgrade().is_some_and(|o| &o == s)) {
            v.push(s.downgrade());
        }
    }

    /// Live registered `GSettings` objects.
    pub fn live_settings(&self) -> Vec<gio::Settings> {
        self.settings.borrow().iter().filter_map(|w| w.upgrade()).collect()
    }
}

/// Canonical state name: the optional `app-state/` prefix is dropped; the rest
/// is `/`-separated segments of `[A-Za-z0-9_.-]`, none empty, `.` or `..`.
pub fn normalize_name(name: &str) -> Result<String, String> {
    let n = name.strip_prefix(STATE_PREFIX).unwrap_or(name);
    if n.is_empty() || n.len() > 128 {
        return Err(format!("state name '{name}' must be 1..=128 bytes after the '{STATE_PREFIX}' prefix"));
    }
    for seg in n.split('/') {
        let ok = !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if !ok {
            return Err(format!("state name '{name}': segment '{seg}' is not [A-Za-z0-9_.-]+ (and not '.' or '..')"));
        }
    }
    Ok(n.to_owned())
}

/// Expose app state as the read-only MCP resource `app-state/<name>`. The
/// closure runs on the main thread when a client reads the resource.
/// Re-registering a name replaces the previous closure. A no-op unless the
/// agent is attached and active; call it on the main thread.
pub fn register_state<F: Fn() -> Value + 'static>(name: &str, read: F) {
    let Some(agent) = Agent::current() else { return };
    match normalize_name(name) {
        Ok(n) => agent.app_state.register(n, Rc::new(read)),
        Err(e) => eprintln!("gtk-mcp: register_state ignored: {e}"),
    }
}

/// Tell the agent about a `GSettings` object the app uses, so
/// `inspect_gsettings` / `set_gsetting` operate on that very object (including
/// delay-apply mode) and report its schema even outside the app-id prefix.
/// The agent keeps a weak reference. A no-op unless the agent is active.
pub fn register_settings(settings: &gio::Settings) {
    if let Some(agent) = Agent::current() {
        agent.app_state.add_settings(settings);
    }
}

/// Handle the app-state wire methods; `None` for any other method.
pub fn handle(agent: &Agent, method: &str, params: &Value) -> Option<Result<Value, AgentError>> {
    match method {
        state_method::LIST => Some(Ok(json!({ "states": agent.app_state.names() }))),
        state_method::READ => Some(match params.get("name").and_then(Value::as_str) {
            None => Err(AgentError::invalid_params("state/read needs `name`")),
            Some(name) => agent.app_state.read(name).map(|value| json!({ "name": name, "value": value })),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(normalize_name("app-state/panels").unwrap(), "panels");
        assert_eq!(normalize_name("panels/lock-1").unwrap(), "panels/lock-1");
        for bad in ["", "app-state/", "a//b", "../x", "a/./b", "a b", "a?b"] {
            assert!(normalize_name(bad).is_err(), "{bad}");
        }
    }

    /// gm-kvv: replacing a registration drops the old closure; whatever its
    /// drop runs (a C caller's GDestroyNotify) may use the registry again.
    #[test]
    fn replaced_closure_is_dropped_outside_the_registry_borrow() {
        struct OnDrop(Rc<AppState>, Rc<std::cell::Cell<bool>>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                assert_eq!(self.0.names(), vec!["a".to_string()]);
                self.0.register("b".into(), Rc::new(|| json!(2)));
                self.1.set(true);
            }
        }
        let state = Rc::new(AppState::default());
        let dropped = Rc::new(std::cell::Cell::new(false));
        let guard = OnDrop(state.clone(), dropped.clone());
        state.register("a".into(), Rc::new(move || {
            let _ = &guard;
            json!(1)
        }));
        state.register("a".into(), Rc::new(|| json!(3)));
        assert!(dropped.get(), "the replaced closure was dropped");
        assert_eq!(state.names(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(state.read("a").unwrap(), json!(3));
    }
}
