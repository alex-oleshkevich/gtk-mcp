//! `inspect_layout` and `audit_layout` (tools.md layout-style, plan AC-12).
//!
//! Everything is read through public GTK API: `gtk_widget_measure`,
//! `gtk_widget_get_allocation`, `gtk_widget_compute_bounds`, the label's pango
//! layout and the icon theme. Audit rules judge mapped widgets only (what a
//! human can currently see); detached adapter content is not audited.

use crate::agent::Agent;
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::graphene;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, AuditLayoutParams, ErrorCode, InspectLayoutParams, LayoutCheck};
use serde_json::{Map, Value, json};
use std::rc::Rc;

const DEFAULT_AUDIT_LIMIT: usize = 100;
const MAX_SIBLINGS: usize = 50;
const MAX_OVERLAP_CHILDREN: usize = 200;
/// Sub-pixel slack for float bounds.
const TOLERANCE: f32 = 0.75;
/// Overlaps thinner than this (in either direction) are rounding noise.
const MIN_OVERLAP: f32 = 1.0;

fn rect_json(r: &graphene::Rect) -> Value {
    json!({"x": r.x(), "y": r.y(), "width": r.width(), "height": r.height()})
}

fn align_name(a: gtk::Align) -> &'static str {
    match a {
        gtk::Align::Fill => "fill",
        gtk::Align::Start => "start",
        gtk::Align::End => "end",
        gtk::Align::Center => "center",
        gtk::Align::Baseline => "baseline",
        _ => "unknown",
    }
}

fn orientation_name(o: gtk::Orientation) -> &'static str {
    if o == gtk::Orientation::Horizontal { "horizontal" } else { "vertical" }
}

fn request_mode_name(m: gtk::SizeRequestMode) -> &'static str {
    match m {
        gtk::SizeRequestMode::HeightForWidth => "height_for_width",
        gtk::SizeRequestMode::WidthForHeight => "width_for_height",
        gtk::SizeRequestMode::ConstantSize => "constant_size",
        _ => "unknown",
    }
}

fn measure(w: &gtk::Widget, o: gtk::Orientation, for_size: i32) -> Value {
    let (minimum, natural, minimum_baseline, natural_baseline) = w.measure(o, for_size);
    json!({
        "orientation": orientation_name(o),
        "for_size": for_size,
        "minimum": minimum,
        "natural": natural,
        "minimum_baseline": minimum_baseline,
        "natural_baseline": natural_baseline,
    })
}

fn allocation_json(w: &gtk::Widget) -> Value {
    let a = w.allocation();
    json!({"x": a.x(), "y": a.y(), "width": a.width(), "height": a.height()})
}

fn brief(agent: &Agent, w: &gtk::Widget) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ref".into(), json!(agent.registry.ref_for(w)));
    m.insert("type".into(), json!(widgets::type_name(w)));
    if let Some(id) = widgets::automation_id(w) {
        m.insert("automation_id".into(), json!(id));
    }
    m
}

#[allow(deprecated)]
pub fn inspect_layout(agent: &Rc<Agent>, p: InspectLayoutParams) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, true)?;
    let mut m = Map::new();
    m.insert("ref".into(), json!(agent.registry.ref_for(&w)));
    m.insert("type".into(), json!(widgets::type_name(&w)));
    m.insert("state".into(), json!(crate::registry::widget_state(&w).as_str()));
    m.insert("visible".into(), json!(w.is_visible()));
    m.insert("mapped".into(), json!(w.is_mapped()));
    m.insert("child_visible".into(), json!(w.is_child_visible()));

    m.insert("allocation".into(), allocation_json(&w));
    m.insert("baseline".into(), json!(w.allocated_baseline()));
    m.insert("size_request".into(), {
        let (width, height) = w.size_request();
        json!({"width": width, "height": height})
    });
    let root_bounds = w.root().and_then(|r| w.compute_bounds(r.upcast_ref::<gtk::Widget>()));
    m.insert("bounds_in_root".into(), root_bounds.as_ref().map_or(Value::Null, rect_json));
    m.insert("bounds_own".into(), w.compute_bounds(&w).as_ref().map_or(Value::Null, rect_json));
    m.insert(
        "margins".into(),
        json!({"top": w.margin_top(), "bottom": w.margin_bottom(), "start": w.margin_start(), "end": w.margin_end()}),
    );
    m.insert("alignment".into(), json!({"halign": align_name(w.halign()), "valign": align_name(w.valign())}));
    m.insert(
        "expand".into(),
        json!({
            "hexpand": w.hexpands(),
            "vexpand": w.vexpands(),
            "hexpand_set": w.is_hexpand_set(),
            "vexpand_set": w.is_vexpand_set(),
            "compute_hexpand": w.compute_expand(gtk::Orientation::Horizontal),
            "compute_vexpand": w.compute_expand(gtk::Orientation::Vertical),
        }),
    );
    m.insert("overflow".into(), json!(if w.overflow() == gtk::Overflow::Hidden { "hidden" } else { "visible" }));
    m.insert("request_mode".into(), json!(request_mode_name(w.request_mode())));
    m.insert("layout_manager".into(), json!(w.layout_manager().map(|l| l.type_().name())));
    if let Some(o) = w.dynamic_cast_ref::<gtk::Orientable>() {
        m.insert("orientation".into(), json!(orientation_name(o.orientation())));
    }
    m.insert(
        "measure".into(),
        json!({
            "horizontal": measure(&w, gtk::Orientation::Horizontal, -1),
            "vertical": measure(&w, gtk::Orientation::Vertical, -1),
        }),
    );
    if let Some(size) = p.for_size {
        if size < 0 {
            return Err(AgentError::new(ErrorCode::InvalidValue, "for_size must be >= 0"));
        }
        m.insert(
            "for_size".into(),
            json!({
                "size": size,
                // height requested for a width of `size`
                "as_width": measure(&w, gtk::Orientation::Vertical, size),
                // width requested for a height of `size`
                "as_height": measure(&w, gtk::Orientation::Horizontal, size),
            }),
        );
    }

    if let Some(parent) = w.parent() {
        let mut pj = brief(agent, &parent);
        pj.insert("allocation".into(), allocation_json(&parent));
        pj.insert("layout_manager".into(), json!(parent.layout_manager().map(|l| l.type_().name())));
        if let Some(o) = parent.dynamic_cast_ref::<gtk::Orientable>() {
            pj.insert("orientation".into(), json!(orientation_name(o.orientation())));
        }
        m.insert("parent".into(), Value::Object(pj));
        let mut siblings = Vec::new();
        let mut total = 0usize;
        let mut c = parent.first_child();
        while let Some(s) = c {
            c = s.next_sibling();
            total += 1;
            if siblings.len() < MAX_SIBLINGS {
                let mut sj = brief(agent, &s);
                sj.insert("self".into(), json!(s == w));
                sj.insert("allocation".into(), allocation_json(&s));
                sj.insert("visible".into(), json!(s.is_visible()));
                sj.insert("mapped".into(), json!(s.is_mapped()));
                siblings.push(Value::Object(sj));
            }
        }
        m.insert("siblings".into(), Value::Array(siblings));
        m.insert("sibling_count".into(), json!(total));
    } else {
        m.insert("parent".into(), Value::Null);
    }
    Ok(Value::Object(m))
}

// -- audit_layout --------------------------------------------------------------

struct Audit {
    checks: Vec<LayoutCheck>,
    limit: usize,
    total: usize,
    findings: Vec<Value>,
    unsupported: Vec<Value>,
    scanned: usize,
}

impl Audit {
    fn on(&self, c: LayoutCheck) -> bool {
        self.checks.contains(&c)
    }

    fn add(&mut self, f: Value) {
        self.total += 1;
        if self.findings.len() < self.limit {
            self.findings.push(f);
        }
    }
}

/// GTK's and libadwaita's own plumbing: gizmos (scrollbar troughs, handles,
/// `AdwGizmo` row internals, ...) and built-in icons are legitimately
/// zero-sized, overlapping or overflowing.
fn is_internal(w: &gtk::Widget) -> bool {
    let name = w.type_().name();
    name.ends_with("Gizmo") || matches!(name, "GtkBuiltinIcon" | "GtkTextHandle")
}

/// Containers whose children overlap or overflow by design (scrolling
/// containers clip their content, windows stack chrome).
fn skip_geometry_parent(p: &gtk::Widget) -> bool {
    p.is::<gtk::ScrolledWindow>()
        || p.is::<gtk::Scrollable>()
        || p.is::<gtk::Viewport>()
        || p.is::<gtk::Window>()
        || p.is::<gtk::Popover>()
        || is_internal(p)
}

fn finding(agent: &Agent, check: &str, w: &gtk::Widget, message: String, evidence: Value) -> Value {
    let mut m = brief(agent, w);
    m.insert("check".into(), json!(check));
    m.insert("message".into(), json!(message));
    m.insert("evidence".into(), evidence);
    Value::Object(m)
}

fn audit_label(agent: &Agent, a: &mut Audit, w: &gtk::Widget, label: &gtk::Label) {
    if label.ellipsize() == gtk::pango::EllipsizeMode::None || !label.layout().is_ellipsized() {
        return;
    }
    let (_, natural_width, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
    let allocated = w.width();
    // Width of the whole text on one line (the label's own layout is truncated).
    let (text, text_width) = if secrecy::is_secret(w) {
        (secrecy::redacted(), Value::Null)
    } else {
        let full = label.create_pango_layout(Some(&label.text()));
        (json!(label.text().chars().take(120).collect::<String>()), json!(full.pixel_size().0))
    };
    let ellipsize = glib::EnumValue::from_value(&label.ellipsize().to_value()).map(|(_, v)| v.nick().to_string());
    let msg = format!("label text is truncated with an ellipsis ({allocated}px allocated)");
    a.add(finding(
        agent,
        "ellipsized_label",
        w,
        msg,
        json!({
            "text": text,
            "ellipsize": ellipsize,
            "allocated_width": allocated,
            "natural_width": natural_width,
            "text_width": text_width,
            "max_width_chars": label.max_width_chars(),
            "width_chars": label.width_chars(),
            "lines": label.lines(),
            "wrap": label.wraps(),
        }),
    ));
}

/// Icon names that no theme in the widget's display resolves.
fn icon_names(w: &gtk::Widget) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(img) = w.downcast_ref::<gtk::Image>() {
        if let Some(n) = img.icon_name() {
            names.push(n.to_string());
        } else if let Some(g) = img.gicon() {
            if let Some(t) = g.downcast_ref::<gio::ThemedIcon>() {
                names.extend(t.names().iter().map(|s| s.to_string()));
            }
        }
    } else if let Some(b) = w.downcast_ref::<gtk::Button>() {
        names.extend(b.icon_name().map(|n| n.to_string()));
    } else if let Some(b) = w.downcast_ref::<gtk::MenuButton>() {
        names.extend(b.icon_name().map(|n| n.to_string()));
    }
    names.retain(|n| !n.is_empty() && !n.starts_with('/'));
    names
}

fn audit_icon(agent: &Agent, a: &mut Audit, w: &gtk::Widget) {
    let names = icon_names(w);
    if names.is_empty() {
        return;
    }
    let theme = gtk::IconTheme::for_display(&w.display());
    // A themed icon with fallbacks resolves when any of its names does.
    if names.iter().any(|n| theme.has_icon(n)) {
        return;
    }
    a.add(finding(
        agent,
        "unresolved_icon",
        w,
        format!("icon name '{}' is not found in the icon theme", names[0]),
        json!({"icon_names": names, "icon_theme": gtk::Settings::for_display(&w.display()).gtk_icon_theme_name().map(|n| n.to_string())}),
    ));
}

fn audit_widget(agent: &Agent, a: &mut Audit, w: &gtk::Widget) {
    a.scanned += 1;
    if let Some(label) = w.downcast_ref::<gtk::Label>() {
        if a.on(LayoutCheck::EllipsizedLabel) {
            audit_label(agent, a, w, label);
        }
    }
    if a.on(LayoutCheck::UnresolvedIcon) {
        audit_icon(agent, a, w);
    }
    if a.on(LayoutCheck::InscriptionEllipsis) && w.type_().name() == "GtkInscription" {
        let overflow = w.find_property("text-overflow").and_then(|ps| {
            glib::EnumValue::from_value(&w.property_value(ps.name())).map(|(_, v)| v.nick().to_string())
        });
        if overflow.as_deref().is_some_and(|n| n.starts_with("ellipsize")) {
            a.unsupported.push(json!({
                "check": "ellipsized_label",
                "ref": agent.registry.ref_for(w),
                "type": "GtkInscription",
                "text_overflow": overflow,
                "reason": "GtkInscription exposes no pango layout, so truncation cannot be detected",
            }));
        }
    }
    let Some(parent) = w.parent() else { return };
    if is_internal(w) || skip_geometry_parent(&parent) {
        return;
    }
    // A collapsed revealer is meant to have no size.
    let collapsed = w.downcast_ref::<gtk::Revealer>().is_some_and(|r| !r.reveals_child());
    if a.on(LayoutCheck::ZeroSize) && (w.width() == 0 || w.height() == 0) && !collapsed {
        let (req_w, req_h) = w.size_request();
        let message = format!("widget is visible but allocated {}x{}", w.width(), w.height());
        a.add(finding(
            agent,
            "zero_size",
            w,
            message,
            json!({
                "allocation": allocation_json(w),
                "size_request": {"width": req_w, "height": req_h},
                "parent": agent.registry.ref_for(&parent),
            }),
        ));
    }
    if a.on(LayoutCheck::OutsideParent) && w.width() > 0 && w.height() > 0 {
        if let Some(b) = w.compute_bounds(&parent) {
            let (pw, ph) = (parent.width() as f32, parent.height() as f32);
            let left = (-b.x()).max(0.0);
            let top = (-b.y()).max(0.0);
            let right = (b.x() + b.width() - pw).max(0.0);
            let bottom = (b.y() + b.height() - ph).max(0.0);
            if left > TOLERANCE || top > TOLERANCE || right > TOLERANCE || bottom > TOLERANCE {
                let fully = b.x() >= pw || b.y() >= ph || b.x() + b.width() <= 0.0 || b.y() + b.height() <= 0.0;
                let clipped = parent.overflow() == gtk::Overflow::Hidden;
                a.add(finding(
                    agent,
                    "outside_parent",
                    w,
                    format!(
                        "widget extends beyond its parent ({}){}",
                        parent.type_().name(),
                        if clipped { "; the parent clips it" } else { "" }
                    ),
                    json!({
                        "bounds_in_parent": rect_json(&b),
                        "parent": agent.registry.ref_for(&parent),
                        "parent_size": {"width": pw, "height": ph},
                        "overflow_px": {"left": left, "top": top, "right": right, "bottom": bottom},
                        "fully_outside": fully,
                        "parent_clips": clipped,
                    }),
                ));
            }
        }
    }
}

/// Overlap between mapped children of `parent` (reported once per pair).
fn audit_overlaps(agent: &Agent, a: &mut Audit, parent: &gtk::Widget) {
    if skip_geometry_parent(parent) {
        return;
    }
    let mut kids: Vec<(gtk::Widget, graphene::Rect)> = Vec::new();
    let mut c = parent.first_child();
    while let Some(k) = c {
        c = k.next_sibling();
        if kids.len() >= MAX_OVERLAP_CHILDREN {
            break;
        }
        if !k.is_mapped() || is_internal(&k) || k.is::<gtk::Popover>() {
            continue;
        }
        if let Some(b) = k.compute_bounds(parent) {
            if b.width() > 0.0 && b.height() > 0.0 {
                kids.push((k, b));
            }
        }
    }
    for i in 0..kids.len() {
        for j in (i + 1)..kids.len() {
            let (first, fb) = &kids[i];
            let (second, sb) = &kids[j];
            let Some(inter) = fb.intersection(sb) else { continue };
            if inter.width() < MIN_OVERLAP || inter.height() < MIN_OVERLAP {
                continue;
            }
            let mut ev = Map::new();
            ev.insert("other".into(), Value::Object(brief(agent, first)));
            ev.insert("parent".into(), json!(agent.registry.ref_for(parent)));
            ev.insert("parent_type".into(), json!(widgets::type_name(parent)));
            ev.insert("overlap".into(), rect_json(&inter));
            ev.insert("bounds".into(), rect_json(sb));
            ev.insert("other_bounds".into(), rect_json(fb));
            let message = format!(
                "overlaps its sibling {} by {}x{}px",
                agent.registry.ref_for(first),
                inter.width().round(),
                inter.height().round()
            );
            a.add(finding(agent, "overlapping_siblings", second, message, Value::Object(ev)));
        }
    }
}

fn walk(agent: &Agent, a: &mut Audit, w: &gtk::Widget) {
    if !w.is_mapped() {
        return;
    }
    audit_widget(agent, a, w);
    if a.on(LayoutCheck::OverlappingSiblings) {
        audit_overlaps(agent, a, w);
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        c = child.next_sibling();
        walk(agent, a, &child);
    }
}

pub fn audit_layout(agent: &Rc<Agent>, p: AuditLayoutParams) -> Result<Value, AgentError> {
    let roots: Vec<gtk::Widget> = match &p.target {
        Some(t) => vec![selector::resolve_widget(agent, t, true)?],
        None => selector::toplevels().into_iter().filter(|w| w.is_visible()).map(|w| w.upcast()).collect(),
    };
    let checks = p.checks.clone().unwrap_or_else(|| {
        vec![
            LayoutCheck::EllipsizedLabel,
            LayoutCheck::OutsideParent,
            LayoutCheck::ZeroSize,
            LayoutCheck::OverlappingSiblings,
            LayoutCheck::UnresolvedIcon,
            LayoutCheck::InscriptionEllipsis,
        ]
    });
    let mut a = Audit {
        checks,
        limit: p.limit.map_or(DEFAULT_AUDIT_LIMIT, |l| l as usize),
        total: 0,
        findings: Vec::new(),
        unsupported: Vec::new(),
        scanned: 0,
    };
    for root in &roots {
        walk(agent, &mut a, root);
    }
    Ok(json!({
        "scope": "mapped widgets",
        "roots": roots.iter().map(|r| agent.registry.ref_for(r)).collect::<Vec<_>>(),
        "scanned": a.scanned,
        "total": a.total,
        "truncated": a.total > a.findings.len(),
        "findings": a.findings,
        "unsupported": a.unsupported,
    }))
}
