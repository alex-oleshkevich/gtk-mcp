//! Selector and window resolution (plan 7, AC-3). All present keys must match
//! (AND). Single-target tools require exactly one match after `nth`.

use crate::agent::Agent;
use crate::{layer, secrecy, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, Selector, StateFilter, Target, WindowSel};
use serde_json::{Value, json};

/// One selector hit.
pub struct Hit {
    pub widget: gtk::Widget,
    pub attached: bool,
}

pub fn toplevels() -> Vec<gtk::Window> {
    gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()).collect()
}

/// GdkMonitor connector of the monitor showing the window, if mapped.
pub fn monitor_connector(win: &gtk::Window) -> Option<String> {
    let surface = win.surface()?;
    let display = surface.display();
    display.monitor_at_surface(&surface)?.connector().map(Into::into)
}

fn window_matches(agent: &Agent, win: &gtk::Window, sel: &WindowSel) -> Result<bool, AgentError> {
    Ok(match sel {
        WindowSel::Ref(r) => agent.registry.resolve_widget(r)? == *win.upcast_ref::<gtk::Widget>(),
        WindowSel::Title(t) => win.title().as_deref() == Some(t.as_str()),
        WindowSel::Monitor(m) => monitor_connector(win).as_deref() == Some(m.as_str()),
        WindowSel::AppWindowId(id) => win.downcast_ref::<gtk::ApplicationWindow>().is_some_and(|a| a.id() == *id),
        WindowSel::LayerNamespace(ns) => layer::namespace(win).as_deref() == Some(ns.as_str()),
    })
}

/// Windows a selector's `window` key restricts the search to (all toplevels
/// when absent).
pub fn candidate_windows(agent: &Agent, sel: Option<&WindowSel>) -> Result<Vec<gtk::Window>, AgentError> {
    let all = toplevels();
    match sel {
        None => Ok(all),
        Some(s) => {
            let mut v = Vec::new();
            for w in all {
                if window_matches(agent, &w, s)? {
                    v.push(w);
                }
            }
            Ok(v)
        }
    }
}

fn state_matches(w: &gtk::Widget, f: &StateFilter) -> bool {
    let checks: [(&str, Option<bool>); 8] = [
        ("visible", f.visible),
        ("mapped", f.mapped),
        ("sensitive", f.sensitive),
        ("has_focus", f.has_focus),
        ("expanded", f.expanded),
        ("active", f.active),
        ("checked", f.checked),
        ("selected", f.selected),
    ];
    checks.iter().all(|(k, want)| match want {
        None => true,
        Some(want) => widgets::state_flag(w, k) == Some(*want),
    })
}

fn matches_own(w: &gtk::Widget, sel: &Selector) -> bool {
    if let Some(a) = &sel.automation_id {
        if widgets::automation_id(w).as_deref() != Some(a.as_str()) {
            return false;
        }
    }
    if let Some(b) = &sel.buildable_id {
        if widgets::buildable_id(w).as_deref() != Some(b.as_str()) {
            return false;
        }
    }
    if let Some(t) = &sel.type_name {
        if !widgets::type_matches(w, t) {
            return false;
        }
    }
    if let Some(c) = &sel.css_class {
        if !widgets::has_css_class(w, c) {
            return false;
        }
    }
    if sel.text.is_some() || sel.text_contains.is_some() {
        // Secret objects never match text selectors.
        if secrecy::is_secret(w) {
            return false;
        }
        let Some(text) = widgets::selector_text(w) else { return false };
        if let Some(t) = &sel.text {
            if &text != t {
                return false;
            }
        }
        if let Some(t) = &sel.text_contains {
            if !text.contains(t.as_str()) {
                return false;
            }
        }
    }
    if let Some(r) = &sel.role {
        if !widgets::role_name(w).eq_ignore_ascii_case(r) {
            return false;
        }
    }
    if let Some(s) = &sel.state {
        if !state_matches(w, s) {
            return false;
        }
    }
    true
}

fn matches(w: &gtk::Widget, stack: &[gtk::Widget], sel: &Selector) -> bool {
    if !matches_own(w, sel) {
        return false;
    }
    if let Some(anc) = &sel.ancestor {
        return (0..stack.len()).rev().any(|i| matches(&stack[i], &stack[..i], anc));
    }
    true
}

fn walk(w: &gtk::Widget, attached: bool, stack: &mut Vec<gtk::Widget>, f: &mut dyn FnMut(&gtk::Widget, bool, &[gtk::Widget])) {
    f(w, attached, stack);
    stack.push(w.clone());
    for c in widgets::children(w) {
        walk(&c.widget, attached && c.attached, stack, f);
    }
    stack.pop();
}

/// Every widget matching `sel`, in tree order, before `nth` is applied.
pub fn find_all(agent: &Agent, sel: &Selector) -> Result<Vec<Hit>, AgentError> {
    let include_detached = sel.include_detached.unwrap_or(false);
    let mut hits = Vec::new();
    for win in candidate_windows(agent, sel.window.as_ref())? {
        let mut stack = Vec::new();
        walk(win.upcast_ref(), true, &mut stack, &mut |w, attached, stack| {
            if (attached || include_detached) && matches(w, stack, sel) {
                hits.push(Hit { widget: w.clone(), attached });
            }
        });
    }
    Ok(hits)
}

/// Matches after applying `nth`.
pub fn find(agent: &Agent, sel: &Selector) -> Result<Vec<Hit>, AgentError> {
    let mut hits = find_all(agent, sel)?;
    if let Some(n) = sel.nth {
        if n >= hits.len() {
            return Err(AgentError::new(
                ErrorCode::NotFound,
                format!("nth={n} but the selector matched {} widgets", hits.len()),
            ));
        }
        hits = vec![hits.swap_remove(n)];
    }
    Ok(hits)
}

pub fn describe(agent: &Agent, w: &gtk::Widget) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("ref".into(), json!(agent.registry.ref_for(w)));
    m.insert("type".into(), json!(widgets::type_name(w)));
    if !secrecy::is_secret(w) {
        if let Some(t) = widgets::selector_text(w) {
            m.insert("text".into(), json!(t.chars().take(80).collect::<String>()));
        }
    } else {
        m.insert("secret".into(), json!(true));
    }
    if let Some(a) = widgets::automation_id(w) {
        m.insert("automation_id".into(), json!(a));
    }
    Value::Object(m)
}

/// Exactly-one resolution for single-target tools.
pub fn resolve_one(agent: &Agent, sel: &Selector) -> Result<Hit, AgentError> {
    let mut hits = find(agent, sel)?;
    match hits.len() {
        0 => Err(AgentError::new(ErrorCode::NotFound, "the selector matched no widget")),
        1 => Ok(hits.remove(0)),
        n => Err(AgentError::new(
            ErrorCode::AmbiguousTarget,
            format!("the selector matched {n} widgets; add keys or use nth"),
        )
        .with_data(json!({
            "total": n,
            "candidates": hits.iter().take(10).map(|h| describe(agent, &h.widget)).collect::<Vec<_>>(),
        }))),
    }
}

/// Resolve a target to a widget. `need_root` makes detached widgets an error
/// (operations needing a root: interact, capture, layout).
pub fn resolve_widget(agent: &Agent, t: &Target, need_root: bool) -> Result<gtk::Widget, AgentError> {
    let w = match t {
        Target::Ref(r) => agent.registry.resolve_widget(r)?,
        Target::Select(s) => resolve_one(agent, s)?.widget,
    };
    if need_root && crate::registry::widget_state(&w) == crate::registry::ObjState::Detached {
        return Err(AgentError::new(
            ErrorCode::Detached,
            format!("{} is alive but not in a window", agent.registry.ref_for(&w)),
        ));
    }
    Ok(w)
}

/// Resolve a target to any object (refs may name non-widgets, selectors only widgets).
pub fn resolve_object(agent: &Agent, t: &Target) -> Result<glib::Object, AgentError> {
    match t {
        Target::Ref(r) => agent.registry.resolve(r),
        Target::Select(s) => Ok(resolve_one(agent, s)?.widget.upcast()),
    }
}
