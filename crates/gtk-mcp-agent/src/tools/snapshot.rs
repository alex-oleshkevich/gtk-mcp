//! `snapshot`: the canonical JSON widget tree (docs/widget-tree.md). Text is
//! rendered by the broker from the same JSON.

use crate::agent::Agent;
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, SnapshotParams, SnapshotView, Target};
use serde_json::{Map, Value, json};
use std::rc::Rc;

pub const DEFAULT_MAX_NODES: usize = 1000;
pub const DEFAULT_MAX_DEPTH: usize = 64;
pub const DEFAULT_TEXT_LIMIT: usize = 128;

pub struct Limits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_unmapped: bool,
    pub include_closed_popovers: bool,
    pub text_limit: usize,
}

impl Limits {
    pub fn from_params(p: &SnapshotParams) -> Self {
        Self {
            max_depth: p.max_depth.map(|d| d as usize).unwrap_or(DEFAULT_MAX_DEPTH),
            max_nodes: p.max_nodes.map(|d| (d as usize).max(1)).unwrap_or(DEFAULT_MAX_NODES),
            include_unmapped: p.include_unmapped.unwrap_or(true),
            include_closed_popovers: p.include_closed_popovers.unwrap_or(false),
            text_limit: p.text_limit.map(|d| d as usize).unwrap_or(DEFAULT_TEXT_LIMIT),
        }
    }
}

struct Builder<'a> {
    agent: &'a Agent,
    root: gtk::Widget,
    limits: &'a Limits,
    count: usize,
    truncated: bool,
}

fn cut(s: &str, limit: usize) -> (String, bool) {
    if s.chars().count() <= limit {
        (s.to_string(), false)
    } else {
        (s.chars().take(limit).collect(), true)
    }
}

fn is_realized_list(w: &gtk::Widget) -> bool {
    w.is::<gtk::ListView>() || w.is::<gtk::GridView>() || w.is::<gtk::ColumnView>()
}

impl Builder<'_> {
    fn node(&mut self, w: &gtk::Widget, attached: bool, via: Option<&str>, depth: usize, is_root: bool) -> Value {
        self.count += 1;
        let mut m = Map::new();
        m.insert("ref".into(), json!(self.agent.registry.ref_for(w)));
        m.insert("type".into(), json!(widgets::type_name(w)));
        if is_root {
            m.insert("root_kind".into(), json!(if w.is::<gtk::Window>() { "window" } else { "widget" }));
        }
        let identity = widgets::identity(w);
        if identity.as_object().is_some_and(|o| !o.is_empty()) {
            m.insert("identity".into(), identity);
        }
        m.insert("role".into(), json!(widgets::role_name(w)));
        let secret = secrecy::is_secret(w);
        let mut warnings: Vec<String> = Vec::new();
        let textual = widgets::text_of(w).is_some() || widgets::label_of(w).is_some() || widgets::title_of(w).is_some();
        if secret {
            m.insert("secret".into(), json!(true));
            if textual {
                m.insert("text_redacted".into(), json!(true));
            }
        } else {
            if let Some(t) = widgets::title_of(w) {
                let (t, trunc) = cut(&t, self.limits.text_limit);
                m.insert("title".into(), json!(t));
                if trunc {
                    m.insert("text_truncated".into(), json!(true));
                }
            }
            if let Some(l) = widgets::label_of(w) {
                let (l, trunc) = cut(&l, self.limits.text_limit);
                m.insert("label".into(), json!(l));
                if trunc {
                    m.insert("text_truncated".into(), json!(true));
                }
            }
            if let Some(t) = widgets::text_of(w) {
                let (t, trunc) = cut(&t, self.limits.text_limit);
                m.insert("text".into(), json!(t));
                if trunc {
                    m.insert("text_truncated".into(), json!(true));
                }
            }
        }
        if let Some(v) = widgets::range_value(w) {
            m.insert("value".into(), v);
        }
        m.insert("state".into(), Value::Object(widgets::state_map(w)));
        if w.is::<gtk::Popover>() {
            m.insert("native".into(), json!(true));
        }
        if !attached {
            m.insert("attached".into(), json!(false));
            if let Some(v) = via {
                m.insert("via".into(), json!(v));
            }
        }
        if is_realized_list(w) {
            m.insert("children_source".into(), json!("realized"));
        }
        m.insert("allocation".into(), json!({"width": w.width(), "height": w.height()}));
        match w.compute_bounds(&self.root) {
            Some(b) => {
                m.insert(
                    "bounds_in_root".into(),
                    json!({"x": b.x(), "y": b.y(), "width": b.width(), "height": b.height()}),
                );
            }
            None => warnings.push("bounds_in_root unavailable".into()),
        }
        let caps = widgets::capabilities(w);
        if !caps.is_empty() {
            m.insert("capabilities".into(), json!(caps));
        }

        let mut children = Vec::new();
        let kids = widgets::children(w);
        let closed_popover = w.is::<gtk::Popover>() && !w.is_mapped() && !self.limits.include_closed_popovers;
        if closed_popover {
            m.insert("children_collapsed".into(), json!(true));
            m.insert("child_count".into(), json!(widgets::child_count(w)));
        } else if depth >= self.limits.max_depth {
            if !kids.is_empty() {
                m.insert("children_truncated".into(), json!(true));
                m.insert("omitted_children".into(), json!(kids.len()));
                self.truncated = true;
            }
        } else {
            let eligible: Vec<_> = kids.into_iter().filter(|c| self.limits.include_unmapped || c.widget.is_mapped()).collect();
            let total = eligible.len();
            for (i, c) in eligible.into_iter().enumerate() {
                if self.count >= self.limits.max_nodes {
                    m.insert("children_truncated".into(), json!(true));
                    m.insert("omitted_children".into(), json!(total - i));
                    self.truncated = true;
                    break;
                }
                children.push(self.node(&c.widget, attached && c.attached, c.via, depth + 1, false));
            }
        }
        if !warnings.is_empty() {
            m.insert("warnings".into(), json!(warnings));
        }
        m.insert("children".into(), Value::Array(children));
        Value::Object(m)
    }
}

fn interesting(n: &Value) -> bool {
    let o = n.as_object().expect("node");
    if o.contains_key("root_kind") || o.contains_key("secret") || o.contains_key("native") || o.contains_key("attached") {
        return true;
    }
    for k in ["label", "text", "title", "text_redacted", "value", "children_collapsed"] {
        if o.contains_key(k) {
            return true;
        }
    }
    if let Some(s) = o.get("state").and_then(Value::as_object) {
        if ["expanded", "active", "checked", "selected"].iter().any(|k| s.contains_key(*k)) {
            return true;
        }
    }
    o.get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().filter_map(Value::as_str).any(|c| c != "focus" && c != "scroll_to"))
}

/// Semantic view: elide structural nodes, lifting kept descendants to the
/// nearest kept ancestor. Returns the replacement nodes and the number of
/// elided nodes.
fn compress(mut n: Value) -> (Vec<Value>, u64) {
    let kids = std::mem::take(n.get_mut("children").and_then(Value::as_array_mut).expect("children"));
    let mut out = Vec::new();
    let mut elided = 0;
    for k in kids {
        let (nodes, e) = compress(k);
        out.extend(nodes);
        elided += e;
    }
    if interesting(&n) {
        let o = n.as_object_mut().unwrap();
        o.insert("children".into(), Value::Array(out));
        if elided > 0 {
            o.insert("elided_children".into(), json!(elided));
        }
        (vec![n], 0)
    } else {
        (out, elided + 1)
    }
}

pub fn count_nodes(n: &Value) -> usize {
    1 + n["children"].as_array().map(|c| c.iter().map(count_nodes).sum()).unwrap_or(0)
}

/// Build the node tree under `root_widgets` (shared by `snapshot` and `observe`).
pub fn build(agent: &Agent, roots: &[gtk::Widget], limits: &Limits, view: &SnapshotView) -> Value {
    let mut nodes = Vec::new();
    let mut truncated = false;
    let mut count = 0usize;
    for root in roots {
        let mut b = Builder { agent, root: root.clone(), limits, count, truncated: false };
        // The budget is shared across roots.
        let node = b.node(root, true, None, 0, true);
        count = b.count;
        truncated |= b.truncated;
        nodes.push(node);
    }
    if *view == SnapshotView::Semantic {
        let mut sem = Vec::new();
        for n in nodes {
            let (mut v, e) = compress(n);
            // A root is always kept; `e` is therefore 0.
            debug_assert_eq!(e, 0);
            sem.append(&mut v);
        }
        nodes = sem;
    }
    let node_count: usize = nodes.iter().map(count_nodes).sum();
    json!({
        "roots": nodes,
        "node_count": node_count,
        "complete": !truncated,
        "truncated": truncated,
    })
}

pub fn snapshot(agent: &Rc<Agent>, p: SnapshotParams) -> Result<Value, AgentError> {
    let limits = Limits::from_params(&p);
    let view = p.view.clone().unwrap_or_default();
    let roots: Vec<gtk::Widget> = match &p.root {
        Some(Target::Ref(r)) => vec![agent.registry.resolve_widget(r)?],
        Some(Target::Select(s)) => {
            // A selector with only a window key snapshots those windows.
            let only_window = s.window.is_some()
                && s.automation_id.is_none()
                && s.buildable_id.is_none()
                && s.type_name.is_none()
                && s.css_class.is_none()
                && s.text.is_none()
                && s.text_contains.is_none()
                && s.role.is_none()
                && s.state.is_none()
                && s.ancestor.is_none()
                && s.nth.is_none();
            if only_window {
                selector::candidate_windows(agent, s.window.as_ref())?.into_iter().map(|w| w.upcast()).collect()
            } else {
                vec![selector::resolve_one(agent, s)?.widget]
            }
        }
        None => selector::toplevels().into_iter().map(|w| w.upcast()).collect(),
    };
    let roots: Vec<gtk::Widget> =
        roots.into_iter().filter(|w| limits.include_unmapped || w.is_mapped() || p.root.is_some()).collect();
    let seq = agent.next_snapshot_seq();
    let mut v = build(agent, &roots, &limits, &view);
    let o = v.as_object_mut().unwrap();
    o.insert("snapshot_id".into(), json!(format!("snap-{seq}")));
    o.insert("sequence".into(), json!(seq));
    Ok(v)
}
