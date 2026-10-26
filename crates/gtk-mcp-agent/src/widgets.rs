//! Read-only widget facts shared by snapshot, selectors, inspect and interact.
//! Everything libadwaita-specific goes through GType names and GObject
//! properties; the agent never links libadwaita (plan 6).

use crate::secrecy::is_type_name;
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use serde_json::{Map, Value, json};

/// Classes the agent itself adds; hidden from snapshots and selectors.
pub const AGENT_CSS_CLASSES: &[&str] = &["gtk-mcp-highlight"];

pub struct Child {
    pub widget: gtk::Widget,
    /// `false` for detached content listed through an adapter.
    pub attached: bool,
    pub via: Option<&'static str>,
}

/// Children in GTK order, then detached content from adapters
/// (widget-tree.md "Detached content"; only verified GTK behaviour).
pub fn children(w: &gtk::Widget) -> Vec<Child> {
    let mut out = Vec::new();
    let mut c = w.first_child();
    while let Some(child) = c {
        c = child.next_sibling();
        out.push(Child { widget: child, attached: true, via: None });
    }
    if let Some(exp) = w.downcast_ref::<gtk::Expander>() {
        if let Some(child) = exp.child() {
            if child.parent().is_none() {
                out.push(Child { widget: child, attached: false, via: Some("GtkExpander:child") });
            }
        }
    }
    out
}

pub fn child_count(w: &gtk::Widget) -> usize {
    let mut n = 0;
    let mut c = w.first_child();
    while let Some(child) = c {
        n += 1;
        c = child.next_sibling();
    }
    n
}

pub fn type_name(o: &impl IsA<glib::Object>) -> String {
    o.type_().name().to_string()
}

/// GType names of `o` and all ancestors, most derived first.
pub fn ancestry(o: &impl IsA<glib::Object>) -> Vec<String> {
    let mut v = Vec::new();
    let mut t = Some(o.type_());
    while let Some(ty) = t {
        v.push(ty.name().to_string());
        t = ty.parent();
    }
    v
}

pub fn interfaces(o: &impl IsA<glib::Object>) -> Vec<String> {
    o.type_().interfaces().iter().map(|t| t.name().to_string()).collect()
}

/// Does the widget's type or any ancestor type have this GType name?
pub fn type_matches(w: &impl IsA<glib::Object>, name: &str) -> bool {
    let mut t = Some(w.type_());
    while let Some(ty) = t {
        if ty.name() == name {
            return true;
        }
        t = ty.parent();
    }
    false
}

/// The widget name when the application set one explicitly. GTK returns the
/// type name when none is set, and that case is omitted.
pub fn automation_id(w: &gtk::Widget) -> Option<String> {
    let name = w.widget_name();
    if name.is_empty() || name == w.type_().name() { None } else { Some(name.to_string()) }
}

pub fn buildable_id(w: &gtk::Widget) -> Option<String> {
    w.dynamic_cast_ref::<gtk::Buildable>().and_then(|b| b.buildable_id()).map(|s| s.to_string())
}

pub fn css_classes(w: &gtk::Widget) -> Vec<String> {
    w.css_classes().iter().map(|s| s.to_string()).filter(|c| !AGENT_CSS_CLASSES.contains(&c.as_str())).collect()
}

pub fn has_css_class(w: &gtk::Widget, c: &str) -> bool {
    !AGENT_CSS_CLASSES.contains(&c) && w.has_css_class(c)
}

pub fn role_name(w: &gtk::Widget) -> String {
    let role = w.accessible_role();
    glib::EnumValue::from_value(&role.to_value()).map(|(_, v)| v.nick().replace('-', "_")).unwrap_or_default()
}

pub fn identity(w: &gtk::Widget) -> Value {
    let mut m = Map::new();
    if let Some(b) = buildable_id(w) {
        m.insert("buildable_id".into(), json!(b));
    }
    if let Some(a) = automation_id(w) {
        m.insert("automation_id".into(), json!(a));
    }
    m.insert("css_name".into(), json!(w.css_name().to_string()));
    let classes = css_classes(w);
    if !classes.is_empty() {
        m.insert("css_classes".into(), json!(classes));
    }
    Value::Object(m)
}

fn string_prop(w: &impl IsA<glib::Object>, name: &str) -> Option<String> {
    w.find_property(name)?;
    w.property::<Option<String>>(name)
}

/// The widget's own label (button label, expander label, row title).
pub fn label_of(w: &gtk::Widget) -> Option<String> {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        return b.label().map(Into::into);
    }
    if let Some(b) = w.downcast_ref::<gtk::CheckButton>() {
        return b.label().map(Into::into);
    }
    if let Some(b) = w.downcast_ref::<gtk::Expander>() {
        return b.label().map(Into::into);
    }
    if let Some(b) = w.downcast_ref::<gtk::MenuButton>() {
        return b.label().map(Into::into);
    }
    if let Some(b) = w.downcast_ref::<gtk::Frame>() {
        return b.label().map(Into::into);
    }
    if is_type_name(w, "AdwPreferencesRow") || is_type_name(w, "AdwExpanderRow") || is_type_name(w, "AdwActionRow") {
        return string_prop(w, "title");
    }
    None
}

/// Text content for textual widgets (labels, entries, text views).
pub fn text_of(w: &gtk::Widget) -> Option<String> {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return Some(l.text().to_string());
    }
    if let Some(e) = w.dynamic_cast_ref::<gtk::Editable>() {
        return Some(e.text().to_string());
    }
    if let Some(t) = w.downcast_ref::<gtk::TextView>() {
        let b = t.buffer();
        let (s, e) = b.bounds();
        return Some(b.text(&s, &e, false).to_string());
    }
    if let Some(i) = w.downcast_ref::<gtk::Inscription>() {
        return i.text().map(Into::into);
    }
    None
}

pub fn title_of(w: &gtk::Widget) -> Option<String> {
    w.downcast_ref::<gtk::Window>().and_then(|win| win.title()).map(Into::into)
}

/// Text used by `text` / `text_contains` selectors. A label that merely
/// displays its parent's own label (the internal `GtkLabel` of a button) does
/// not count, so a button matches once.
pub fn selector_text(w: &gtk::Widget) -> Option<String> {
    if let Some(l) = label_of(w) {
        return Some(l);
    }
    if let Some(t) = title_of(w) {
        return Some(t);
    }
    let text = text_of(w)?;
    if w.is::<gtk::Label>() {
        if let Some(parent) = w.parent() {
            if label_of(&parent).as_deref() == Some(text.as_str()) {
                return None;
            }
        }
    }
    Some(text)
}

pub fn is_expanded(w: &gtk::Widget) -> Option<bool> {
    if let Some(e) = w.downcast_ref::<gtk::Expander>() {
        return Some(e.is_expanded());
    }
    if is_type_name(w, "AdwExpanderRow") {
        return Some(w.property::<bool>("expanded"));
    }
    None
}

/// `active` applies to switches and toggles; `checked` to check/radio buttons.
pub fn is_active(w: &gtk::Widget) -> Option<bool> {
    if let Some(s) = w.downcast_ref::<gtk::Switch>() {
        return Some(s.is_active());
    }
    if let Some(t) = w.downcast_ref::<gtk::ToggleButton>() {
        return Some(t.is_active());
    }
    if is_type_name(w, "AdwSwitchRow") {
        return Some(w.property::<bool>("active"));
    }
    None
}

pub fn is_checked(w: &gtk::Widget) -> Option<bool> {
    w.downcast_ref::<gtk::CheckButton>().map(|c| c.is_active())
}

pub fn is_selected(w: &gtk::Widget) -> Option<bool> {
    w.downcast_ref::<gtk::ListBoxRow>().map(|r| r.is_selected())
}

/// One state flag by name, `None` when inapplicable.
pub fn state_flag(w: &gtk::Widget, key: &str) -> Option<bool> {
    match key {
        "visible" => Some(w.is_visible()),
        "mapped" => Some(w.is_mapped()),
        "sensitive" => Some(w.is_sensitive()),
        "focusable" => Some(w.is_focusable()),
        "has_focus" => Some(w.has_focus()),
        "expanded" => is_expanded(w),
        "active" => is_active(w),
        "checked" => is_checked(w),
        "selected" => is_selected(w),
        _ => None,
    }
}

pub fn state_map(w: &gtk::Widget) -> Map<String, Value> {
    let mut m = Map::new();
    for k in ["visible", "mapped", "sensitive", "focusable", "has_focus", "expanded", "active", "checked", "selected"] {
        if let Some(v) = state_flag(w, k) {
            m.insert(k.into(), json!(v));
        }
    }
    m
}

pub fn range_value(w: &gtk::Widget) -> Option<Value> {
    if let Some(r) = w.dynamic_cast_ref::<gtk::Range>() {
        let a = r.adjustment();
        return Some(json!({"current": a.value(), "min": a.lower(), "max": a.upper()}));
    }
    if let Some(s) = w.downcast_ref::<gtk::SpinButton>() {
        let a = s.adjustment();
        return Some(json!({"current": a.value(), "min": a.lower(), "max": a.upper()}));
    }
    None
}

/// Semantic operations the widget supports; consumed by `interact`.
pub fn capabilities(w: &gtk::Widget) -> Vec<&'static str> {
    let mut c: Vec<&'static str> = Vec::new();
    let mut add = |s: &'static str| {
        if !c.contains(&s) {
            c.push(s)
        }
    };
    if w.downcast_ref::<gtk::Button>().is_some() {
        add("activate");
    }
    if w.is::<gtk::ToggleButton>() || w.is::<gtk::Switch>() || is_type_name(w, "AdwSwitchRow") {
        add("set_active");
        add("activate");
    }
    if w.is::<gtk::CheckButton>() {
        add("set_checked");
        add("activate");
    }
    if w.is::<gtk::Expander>() || is_type_name(w, "AdwExpanderRow") {
        add("set_expanded");
    }
    if w.dynamic_cast_ref::<gtk::Editable>().is_some() {
        add("fill");
    }
    if w.is::<gtk::TextView>() {
        add("fill");
    }
    if w.is::<gtk::Range>() || w.is::<gtk::SpinButton>() || is_type_name(w, "AdwSpinRow") {
        add("set_value");
    }
    if w.downcast_ref::<gtk::ListBoxRow>().is_some_and(|r| r.is_activatable())
        || w.is::<gtk::Entry>()
        || w.is::<gtk::PasswordEntry>()
        || w.is::<gtk::SearchEntry>()
    {
        add("activate");
    }
    if w.is::<gtk::DropDown>() || is_type_name(w, "AdwComboRow") || w.is::<gtk::ListBox>() {
        add("select");
    }
    if w.is::<gtk::Stack>() || w.is::<gtk::Notebook>() || is_type_name(w, "AdwViewStack") {
        add("select_page");
    }
    if w.is::<gtk::MenuButton>() || w.is::<gtk::Popover>() || w.is::<gtk::DropDown>() {
        add("popup");
        add("popdown");
    }
    if w.is::<gtk::ScrolledWindow>() {
        add("scroll_by");
    }
    if w.is::<gtk::Window>() {
        add("close");
    }
    if w.is::<gtk::Video>() || w.is::<gtk::MediaControls>() {
        add("play");
        add("pause");
        add("seek");
    }
    if w.is::<gtk::ListView>() || w.is::<gtk::GridView>() || w.is::<gtk::ColumnView>() {
        add("select");
        add("scroll_to");
    }
    if w.is_focusable() || w.dynamic_cast_ref::<gtk::Editable>().is_some() {
        add("focus");
    }
    if !w.is::<gtk::Window>() {
        add("scroll_to");
    }
    c
}
