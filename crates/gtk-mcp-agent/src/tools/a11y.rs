//! `accessible_tree`, `inspect_accessible`, `trace_focus_order` (tools.md a11y).
//!
//! The agent only reports what `GtkAccessible` exposes publicly: role, accessible
//! parent/children, bounds and platform state. Name, description, states and
//! relations have no in-process getter; the broker adds them from AT-SPI
//! (plan 3.2, 8). Nodes also carry the widget facts the broker's audit rules
//! need (label, tooltip, icon name, sensitivity, mapping, click gestures);
//! secret widgets carry none of them.

use crate::agent::Agent;
use crate::tools::shortcuts::controllers_of;
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use glib::translate::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::prelude::GtkWindowExt;
use gtk_mcp_protocol::{AccessibleTreeParams, AgentError, ErrorCode, InspectAccessibleParams, TraceFocusOrderParams};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const DEFAULT_MAX_DEPTH: usize = 64;
const DEFAULT_MAX_NODES: usize = 2000;
const DEFAULT_MAX_STEPS: usize = 200;

fn role_nick(a: &gtk::Accessible) -> String {
    let role = a.accessible_role();
    glib::EnumValue::from_value(&role.to_value()).map(|(_, v)| v.nick().to_string()).unwrap_or_default()
}

fn bounds_json(a: &gtk::Accessible) -> Value {
    match a.bounds() {
        Some((x, y, w, h)) => json!({"x": x, "y": y, "width": w, "height": h}),
        None => Value::Null,
    }
}

fn platform_state(a: &gtk::Accessible) -> Value {
    json!({
        "focusable": a.platform_state(gtk::AccessiblePlatformState::Focusable),
        "focused": a.platform_state(gtk::AccessiblePlatformState::Focused),
        "active": a.platform_state(gtk::AccessiblePlatformState::Active),
    })
}

fn icon_name(w: &gtk::Widget) -> Option<String> {
    for class in ["GtkButton", "GtkMenuButton", "GtkImage"] {
        if widgets::type_matches(w, class) {
            w.find_property("icon-name")?;
            return w.property::<Option<String>>("icon-name").filter(|s| !s.is_empty());
        }
    }
    None
}

struct ClickControllers {
    defaults: HashMap<glib::Type, usize>,
    selectable_label: Option<HashMap<glib::Type, usize>>,
}

thread_local! {
    static DEFAULT_CLICK_CONTROLLERS: RefCell<HashMap<glib::Type, Option<ClickControllers>>> = RefCell::new(HashMap::new());
}

fn click_controller_counts(w: &gtk::Widget) -> HashMap<glib::Type, usize> {
    let mut counts = HashMap::new();
    for c in controllers_of(w) {
        if c.is::<gtk::GestureSingle>() {
            *counts.entry(c.type_()).or_default() += 1;
        }
    }
    counts
}

fn default_click_controllers(type_: glib::Type) -> Option<ClickControllers> {
    // Roots/natives can register toplevels or create surfaces during construction.
    // Initializable types need their own constructor, not a bare g_object_new().
    if type_.is_a(gtk::Native::static_type()) || type_.is_a(gtk::Root::static_type())
        || type_.is_a(gio::Initable::static_type()) || type_.is_a(gio::AsyncInitable::static_type())
    {
        return None;
    }
    // SAFETY: the type comes from a live GtkWidget. Check its construction flags
    // before creating a temporary unparented instance, and sink its floating ref
    // just as glib::Object::with_type does. A failed constructor is not evidence
    // of an application-added gesture.
    let sample: gtk::Widget = unsafe {
        if glib::gobject_ffi::g_type_test_flags(type_.into_glib(), glib::gobject_ffi::G_TYPE_FLAG_ABSTRACT) != 0
            || glib::gobject_ffi::g_type_test_flags(type_.into_glib(), glib::gobject_ffi::G_TYPE_FLAG_INSTANTIATABLE) == 0
        {
            return None;
        }
        let ptr = glib::gobject_ffi::g_object_new_with_properties(type_.into_glib(), 0, std::ptr::null_mut(), std::ptr::null());
        if ptr.is_null() {
            return None;
        }
        let object: glib::Object = from_glib_none(ptr);
        object.downcast().ok()?
    };
    let defaults = click_controller_counts(&sample);
    // Labels install selection/link gestures on demand, rather than in init.
    // Measure that GTK-owned set too, using the same throwaway instance.
    let selectable_label = sample.downcast_ref::<gtk::Label>().map(|label| {
        label.set_selectable(true);
        click_controller_counts(&sample)
    });
    Some(ClickControllers { defaults, selectable_label })
}

fn is_internal_click_target(w: &gtk::Widget) -> bool {
    // Private list rows acquire gestures when a model/factory configures them.
    if widgets::type_matches(w, "GtkListItemBase") {
        return true;
    }
    if !w.type_().name().starts_with("Gtk") {
        return false;
    }
    let mut child = w.clone();
    while let Some(parent) = child.parent() {
        // Explicit application identities distinguish app content from unnamed
        // implementation widgets inside a GTK composite.
        if widgets::automation_id(&child).is_some() || widgets::buildable_id(&child).is_some() {
            return false;
        }
        if parent.is::<gtk::SpinButton>() || parent.is::<gtk::DropDown>() {
            return true;
        }
        if let Some(expander) = parent.downcast_ref::<gtk::Expander>() {
            return expander.child().as_ref() != Some(&child) && expander.label_widget().as_ref() != Some(&child);
        }
        child = parent;
    }
    false
}

/// Only extra press-based gestures count as application click handling. GTK's
/// own gestures (selection, scrolling, window management, etc.) are not controls
/// that the application forgot to expose to assistive technology.
fn has_click_controller(w: &gtk::Widget) -> bool {
    if is_internal_click_target(w) {
        return false;
    }
    let counts = click_controller_counts(w);
    if counts.is_empty() {
        return false;
    }
    DEFAULT_CLICK_CONTROLLERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let Some(baseline) = cache.entry(w.type_()).or_insert_with(|| default_click_controllers(w.type_())) else {
            return false;
        };
        let defaults = if w.downcast_ref::<gtk::Label>().is_some_and(|l| l.is_selectable() || l.has_css_class("link")) {
            baseline.selectable_label.as_ref().unwrap_or(&baseline.defaults)
        } else {
            &baseline.defaults
        };
        counts.iter().any(|(type_, count)| *count > defaults.get(type_).copied().unwrap_or(0))
    })
}

fn node_facts(agent: &Agent, a: &gtk::Accessible) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ref".into(), json!(agent.registry.ref_for(a)));
    m.insert("type".into(), json!(widgets::type_name(a)));
    m.insert("role".into(), json!(role_nick(a)));
    m.insert("bounds".into(), bounds_json(a));
    m.insert("platform_state".into(), platform_state(a));
    if let Some(w) = a.dynamic_cast_ref::<gtk::Widget>() {
        let secret = secrecy::is_secret(w);
        if secret {
            m.insert("secret".into(), json!(true));
        }
        let ident = widgets::identity(w);
        if let Some(id) = ident.get("buildable_id") {
            m.insert("buildable_id".into(), id.clone());
        }
        if let Some(id) = ident.get("automation_id") {
            m.insert("automation_id".into(), id.clone());
        }
        m.insert("sensitive".into(), json!(w.is_sensitive()));
        m.insert("mapped".into(), json!(w.is_mapped()));
        if !secret {
            if let Some(l) = widgets::label_of(w).or_else(|| widgets::title_of(w)) {
                m.insert("label".into(), json!(l));
            }
            if let Some(t) = w.tooltip_text().or_else(|| w.tooltip_markup()) {
                m.insert("tooltip".into(), json!(t.to_string()));
            }
            if let Some(i) = icon_name(w) {
                m.insert("icon_name".into(), json!(i));
            }
            if has_click_controller(w) {
                m.insert("clickable".into(), json!(true));
            }
        }
    }
    m
}

struct Walk<'a> {
    agent: &'a Agent,
    max_depth: usize,
    max_nodes: usize,
    count: usize,
    truncated: bool,
}

impl Walk<'_> {
    fn node(&mut self, a: &gtk::Accessible, depth: usize) -> Value {
        self.count += 1;
        let mut m = node_facts(self.agent, a);
        let mut kids = Vec::new();
        let mut next = a.first_accessible_child();
        if depth >= self.max_depth {
            if next.is_some() {
                self.truncated = true;
                m.insert("children_truncated".into(), json!(true));
            }
        } else {
            while let Some(c) = next {
                if self.count >= self.max_nodes {
                    self.truncated = true;
                    m.insert("children_truncated".into(), json!(true));
                    break;
                }
                next = c.next_accessible_sibling();
                kids.push(self.node(&c, depth + 1));
            }
        }
        m.insert("children".into(), Value::Array(kids));
        Value::Object(m)
    }
}

pub fn accessible_tree(agent: &Rc<Agent>, p: AccessibleTreeParams) -> Result<Value, AgentError> {
    let roots: Vec<gtk::Accessible> = match &p.root {
        Some(t) => vec![selector::resolve_widget(agent, t, false)?.upcast()],
        None => selector::toplevels().into_iter().map(|w| w.upcast()).collect(),
    };
    let mut walk = Walk {
        agent,
        max_depth: p.max_depth.map_or(DEFAULT_MAX_DEPTH, |d| d as usize),
        max_nodes: p.max_nodes.map_or(DEFAULT_MAX_NODES, |d| (d as usize).max(1)),
        count: 0,
        truncated: false,
    };
    let nodes: Vec<Value> = roots.iter().map(|r| walk.node(r, 0)).collect();
    Ok(json!({"roots": nodes, "node_count": walk.count, "truncated": walk.truncated, "pid": std::process::id()}))
}

pub fn inspect_accessible(agent: &Rc<Agent>, p: InspectAccessibleParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    let a = obj.downcast::<gtk::Accessible>().map_err(|o| {
        AgentError::new(ErrorCode::UnsupportedOperation, format!("{} is not a GtkAccessible", widgets::type_name(&o)))
    })?;
    let mut m = node_facts(agent, &a);
    m.insert("parent".into(), a.accessible_parent().map_or(Value::Null, |p| json!(agent.registry.ref_for(&p))));
    let mut kids = Vec::new();
    let mut next = a.first_accessible_child();
    while let Some(c) = next {
        next = c.next_accessible_sibling();
        kids.push(json!(agent.registry.ref_for(&c)));
        if kids.len() >= 500 {
            m.insert("children_truncated".into(), json!(true));
            break;
        }
    }
    m.insert("children".into(), Value::Array(kids));
    Ok(Value::Object(m))
}

fn focus_json(agent: &Agent, w: &gtk::Widget, index: usize) -> Value {
    let secret = secrecy::is_secret(w);
    let mut m = json!({
        "index": index,
        "ref": agent.registry.ref_for(w),
        "type": widgets::type_name(w),
        "role": widgets::role_name(w),
    });
    if let Some(id) = widgets::automation_id(w) {
        m["automation_id"] = json!(id);
    }
    if !secret {
        if let Some(l) = widgets::label_of(w) {
            m["label"] = json!(l);
        }
    }
    m
}

fn is_tab_candidate(w: &gtk::Widget) -> bool {
    w.is_focusable() && w.is_mapped() && w.is_sensitive() && w.can_focus()
}

fn collect_candidates(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    if is_tab_candidate(w) {
        out.push(w.clone());
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        c = child.next_sibling();
        collect_candidates(&child, out);
    }
}

/// Walk the Tab chain with `gtk_widget_child_focus` (plan: side effects possible,
/// the previous focus is restored).
pub async fn trace_focus_order(agent: &Rc<Agent>, p: TraceFocusOrderParams, ctx: &crate::agent::Ctx) -> Result<Value, AgentError> {
    let mut scope: Option<gtk::Widget> = None;
    let window: gtk::Window = match &p.root {
        Some(t) => {
            let w = selector::resolve_widget(agent, t, true)?;
            let win = w.root().and_then(|r| r.downcast::<gtk::Window>().ok()).ok_or_else(|| {
                AgentError::new(ErrorCode::Detached, "root is not inside a window".to_string())
            })?;
            if w != *win.upcast_ref::<gtk::Widget>() {
                scope = Some(w);
            }
            win
        }
        None => {
            let tops = selector::toplevels();
            tops.iter().find(|w| w.is_active()).or_else(|| tops.iter().find(|w| w.is_mapped())).cloned().ok_or_else(|| {
                AgentError::new(ErrorCode::TargetUnmapped, "no mapped window".to_string())
            })?
        }
    };
    let max_steps = p.max_steps.map_or(DEFAULT_MAX_STEPS, |s| (s as usize).max(1));
    let previous = GtkWindowExt::focus(&window);
    GtkWindowExt::set_focus(&window, gtk::Widget::NONE);

    let mut order: Vec<gtk::Widget> = Vec::new();
    let mut seen: HashSet<gtk::Widget> = HashSet::new();
    let mut end = "end";
    let mut repeat_of: Option<usize> = None;
    for _ in 0..max_steps {
        // Cancelled: stop walking, but still restore the focus and report what
        // was walked (the focus already moved; plan 4.2 late results).
        if ctx.is_cancelled() {
            end = "cancelled";
            break;
        }
        let before = GtkWindowExt::focus(&window);
        let moved = scope.as_ref().map_or_else(|| window.upcast_ref::<gtk::Widget>(), |s| s).child_focus(gtk::DirectionType::TabForward);
        let now = GtkWindowExt::focus(&window);
        let Some(now) = now.filter(|_| moved) else { break };
        if before.as_ref() == Some(&now) && !order.is_empty() {
            end = "stuck";
            repeat_of = order.iter().position(|w| *w == now);
            break;
        }
        if !seen.insert(now.clone()) {
            repeat_of = order.iter().position(|w| *w == now);
            end = if repeat_of == Some(0) { "loop" } else { "trap" };
            break;
        }
        order.push(now);
        if order.len() == max_steps {
            end = "max_steps";
        }
    }

    let mut candidates = Vec::new();
    collect_candidates(scope.as_ref().unwrap_or_else(|| window.upcast_ref()), &mut candidates);
    // Entries focus their inner `GtkText`; the entry itself counts as reached.
    let reached = |w: &gtk::Widget| {
        seen.contains(w) || seen.iter().any(|f| f.ancestor(w.type_()).as_ref() == Some(w) && f != w)
    };
    // A walk cut short (`max_steps`, cancellation) proves nothing about what it did not reach.
    let unreachable: Value = if matches!(end, "max_steps" | "cancelled") {
        Value::Null
    } else {
        Value::Array(candidates.iter().filter(|w| !reached(w)).enumerate().map(|(i, w)| focus_json(agent, w, i)).collect())
    };

    GtkWindowExt::set_focus(&window, previous.as_ref());
    let restored = GtkWindowExt::focus(&window) == previous;
    Ok(json!({
        "window": agent.registry.ref_for(&window),
        "scope": scope.as_ref().map(|s| agent.registry.ref_for(s)),
        "order": order.iter().enumerate().map(|(i, w)| focus_json(agent, w, i)).collect::<Vec<_>>(),
        "end": end,
        "loops": end == "loop",
        "trap": end == "trap",
        "repeat_index": repeat_of,
        "unreachable": unreachable,
        "focus_restored": restored,
    }))
}
