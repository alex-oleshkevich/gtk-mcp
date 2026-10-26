//! Action addressing and description shared by the `actions` toolset
//! (tools.md actions; plan 4.3: actions are not objects, they are addressed as
//! `{scope, name}`), the typed GVariant <-> JSON conversion, and the public
//! [`register_action_group`] API (embedding.md "Widget-scoped action groups").
//!
//! GTK 4.24 exports no way to enumerate the groups inserted with
//! `gtk_widget_insert_action_group` (`gtk_widget_list_action_prefixes` /
//! `gtk_widget_get_action_group` are not public API). What is enumerable:
//! the `GtkApplication` (`app.`), the `GtkApplicationWindow` (`win.`),
//! groups registered through [`register_action_group`] and widget-class
//! actions (`gtk_widget_class_query_action`). Everything else still activates
//! by name through `gtk_widget_activate_action`.

use crate::agent::Agent;
use crate::json::{kind, scalar_to_json};
use crate::secrecy;
use gio::prelude::*;
use glib::translate::{FromGlib, IntoGlib};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, Target};
use serde_json::{Map, Value, json};
use std::cell::RefCell;

// -- public registration API ----------------------------------------------------

struct Registered {
    widget: glib::WeakRef<gtk::Widget>,
    prefix: String,
    group: glib::WeakRef<glib::Object>,
}

thread_local! {
    static REGISTERED: RefCell<Vec<Registered>> = const { RefCell::new(Vec::new()) };
}

/// Make a group inserted with `gtk_widget_insert_action_group` listable by
/// `list_actions` (and give `inspect_action` / `activate_action` /
/// `change_action_state` access to its state). Call it right after
/// `insert_action_group`, on the main thread. No-op when the agent is
/// inactive. Re-registering the same `(widget, prefix)` replaces the group.
pub fn register_action_group(widget: &impl IsA<gtk::Widget>, prefix: &str, group: &impl IsA<gio::ActionGroup>) {
    if Agent::current().is_none() {
        return;
    }
    let widget: gtk::Widget = widget.as_ref().clone();
    let prefix = prefix.trim_end_matches('.').to_owned();
    REGISTERED.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|e| e.widget.upgrade().is_some() && e.group.upgrade().is_some());
        r.retain(|e| !(e.prefix == prefix && e.widget.upgrade().as_ref() == Some(&widget)));
        r.push(Registered { widget: widget.downgrade(), prefix, group: group.upcast_ref::<gio::ActionGroup>().upcast_ref::<glib::Object>().downgrade() });
    });
}

fn registered_groups(w: &gtk::Widget) -> Vec<(String, gio::ActionGroup)> {
    REGISTERED.with(|r| {
        r.borrow()
            .iter()
            .filter(|e| e.widget.upgrade().as_ref() == Some(w))
            .filter_map(|e| Some((e.prefix.clone(), e.group.upgrade()?.downcast::<gio::ActionGroup>().ok()?)))
            .collect()
    })
}

/// Groups registered on exactly `w`.
pub fn registered_at(w: &gtk::Widget) -> Vec<GroupRef> {
    registered_groups(w)
        .into_iter()
        .map(|(prefix, group)| GroupRef { group, prefix, kind: "widget", owner: Some(w.clone()), registered: true })
        .collect()
}

// -- scopes ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeKind {
    App,
    Window,
    Widget,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Window => "window",
            Self::Widget => "widget",
        }
    }
}

#[derive(Clone)]
pub struct Scope {
    pub kind: ScopeKind,
    /// `None` for the application scope.
    pub widget: Option<gtk::Widget>,
}

impl Scope {
    pub fn target_ref(&self, agent: &Agent) -> String {
        match &self.widget {
            Some(w) => agent.registry.ref_for(w),
            None => "app".into(),
        }
    }

    pub fn is_secret(&self) -> bool {
        self.widget.as_ref().is_some_and(secrecy::is_secret)
    }

    /// The widget and its ancestors, nearest first (empty for the app scope).
    pub fn chain(&self) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        let mut cur = self.widget.clone();
        while let Some(w) = cur {
            cur = w.parent();
            out.push(w);
        }
        out
    }
}

/// The active `GtkApplicationWindow` (else any, else any window).
pub fn active_window(agent: &Agent) -> Option<gtk::Window> {
    let is_app_win = |w: &gtk::Window| w.is::<gtk::ApplicationWindow>();
    if let Some(w) = agent.app.active_window().filter(is_app_win) {
        return Some(w);
    }
    let wins = agent.app.windows();
    wins.iter().find(|w| is_app_win(w)).cloned().or_else(|| agent.app.active_window()).or_else(|| wins.first().cloned())
}

fn window_scope(agent: &Agent) -> Result<Scope, AgentError> {
    let w = active_window(agent).ok_or_else(|| AgentError::new(ErrorCode::NotFound, "the application has no window"))?;
    Ok(Scope { kind: ScopeKind::Window, widget: Some(w.upcast()) })
}

/// Resolve the `scope` parameter. `hint_prefix` (the prefix of the action
/// name, when known) selects the scope when `scope` is omitted.
pub fn resolve_scope(agent: &Agent, scope: Option<&Target>, hint_prefix: Option<&str>) -> Result<Scope, AgentError> {
    match scope {
        Some(Target::Ref(r)) if r == "app" => Ok(Scope { kind: ScopeKind::App, widget: None }),
        Some(Target::Ref(r)) if r == "window" => window_scope(agent),
        Some(t) => {
            let w = crate::selector::resolve_widget(agent, t, false)?;
            let kind = if w.is::<gtk::Window>() { ScopeKind::Window } else { ScopeKind::Widget };
            Ok(Scope { kind, widget: Some(w) })
        }
        None => match hint_prefix {
            Some("app") => Ok(Scope { kind: ScopeKind::App, widget: None }),
            Some("win") => window_scope(agent),
            Some(p) => Err(AgentError::invalid_params(format!(
                "the action prefix '{p}' needs an explicit scope (a widget ref or selector)"
            ))),
            None => Ok(Scope { kind: ScopeKind::App, widget: None }),
        },
    }
}

/// `{scope, name}` split into scope, prefix and bare action name.
pub struct Addr {
    pub scope: Scope,
    pub prefix: String,
    pub action: String,
}

impl Addr {
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.prefix, self.action)
    }
}

pub fn resolve_addr(agent: &Agent, scope: Option<&Target>, name: &str) -> Result<Addr, AgentError> {
    let (prefix, action) = match name.split_once('.') {
        Some((p, a)) if !p.is_empty() && !a.is_empty() => (Some(p.to_owned()), a.to_owned()),
        Some(_) => return Err(AgentError::invalid_params(format!("'{name}' is not an action name"))),
        None if name.is_empty() => return Err(AgentError::invalid_params("the action name is empty")),
        None => (None, name.to_owned()),
    };
    if scope.is_none() && prefix.is_none() {
        return Err(AgentError::invalid_params(format!("'{name}' has no prefix; pass 'prefix.action' or a scope")));
    }
    let sc = resolve_scope(agent, scope, prefix.as_deref())?;
    let prefix = match (prefix, sc.kind) {
        (Some(p), _) => p,
        (None, ScopeKind::App) => "app".into(),
        (None, ScopeKind::Window) => "win".into(),
        (None, ScopeKind::Widget) => {
            return Err(AgentError::invalid_params(format!("'{name}' has no prefix; widget scopes need 'prefix.action'")));
        }
    };
    if sc.kind == ScopeKind::App && prefix != "app" {
        return Err(AgentError::invalid_params(format!(
            "scope 'app' only holds 'app.' actions; '{prefix}.{action}' needs a window or widget scope"
        )));
    }
    Ok(Addr { scope: sc, prefix, action })
}

// -- groups ---------------------------------------------------------------------

#[derive(Clone)]
pub struct GroupRef {
    pub group: gio::ActionGroup,
    pub prefix: String,
    /// `app`, `window` or `widget`.
    pub kind: &'static str,
    pub owner: Option<gtk::Widget>,
    pub registered: bool,
}

impl GroupRef {
    pub fn source(&self, agent: &Agent) -> Value {
        let mut m = Map::new();
        m.insert("kind".into(), json!(self.kind));
        m.insert("prefix".into(), json!(self.prefix));
        if let Some(o) = &self.owner {
            m.insert("ref".into(), json!(agent.registry.ref_for(o)));
            m.insert("type".into(), json!(o.type_().name()));
        }
        if self.kind == "widget" {
            m.insert("registered".into(), json!(self.registered));
        }
        Value::Object(m)
    }
}

pub fn app_group(agent: &Agent) -> GroupRef {
    GroupRef {
        group: agent.app.clone().upcast::<gio::ActionGroup>(),
        prefix: "app".into(),
        kind: "app",
        owner: None,
        registered: false,
    }
}

fn window_group(win: &gtk::Widget) -> Option<GroupRef> {
    let aw = win.dynamic_cast_ref::<gtk::ApplicationWindow>()?;
    Some(GroupRef { group: aw.clone().upcast(), prefix: "win".into(), kind: "window", owner: Some(win.clone()), registered: false })
}

/// The enumerable group behind `prefix` as seen from `scope`. `Ok(None)`: the
/// prefix is not enumerable from here (unregistered, or no such group).
pub fn find_group(agent: &Agent, scope: &Scope, prefix: &str) -> Result<Option<GroupRef>, AgentError> {
    match prefix {
        "app" => Ok(Some(app_group(agent))),
        "win" => {
            let start = match &scope.widget {
                Some(w) => w.clone(),
                None => return window_scope(agent).map(|s| s.widget.and_then(|w| window_group(&w))),
            };
            Ok(start.ancestor(gtk::ApplicationWindow::static_type()).and_then(|w| window_group(&w)))
        }
        _ => {
            if scope.kind == ScopeKind::App {
                return Err(AgentError::invalid_params(format!(
                    "scope 'app' only holds 'app.' actions; use a widget scope for '{prefix}.'"
                )));
            }
            for w in scope.chain() {
                if let Some((p, g)) = registered_groups(&w).into_iter().find(|(p, _)| p == prefix) {
                    return Ok(Some(GroupRef { group: g, prefix: p, kind: "widget", owner: Some(w), registered: true }));
                }
            }
            Ok(None)
        }
    }
}

// -- widget-class actions -------------------------------------------------------

#[derive(Clone)]
pub struct ClassAction {
    pub owner: glib::Type,
    pub name: String,
    pub parameter_type: Option<String>,
    pub property: Option<String>,
}

/// Actions installed with `gtk_widget_class_install_action` by the widget's
/// class and its parents.
pub fn class_actions(w: &gtk::Widget) -> Vec<ClassAction> {
    let mut out: Vec<ClassAction> = Vec::new();
    let mut ty = Some(w.type_());
    while let Some(t) = ty {
        if !t.is_a(gtk::Widget::static_type()) {
            break;
        }
        // SAFETY: the class is referenced for the duration of the queries and
        // `GtkWidgetClass` is the class struct of every widget type.
        unsafe {
            let class = glib::gobject_ffi::g_type_class_ref(t.into_glib()) as *mut gtk::ffi::GtkWidgetClass;
            let mut i = 0u32;
            loop {
                let mut owner: glib::ffi::GType = 0;
                let mut name: *const std::ffi::c_char = std::ptr::null();
                let mut ptype: *const glib::ffi::GVariantType = std::ptr::null();
                let mut prop: *const std::ffi::c_char = std::ptr::null();
                if gtk::ffi::gtk_widget_class_query_action(class, i, &mut owner, &mut name, &mut ptype, &mut prop) == 0 {
                    break;
                }
                i += 1;
                if name.is_null() {
                    continue;
                }
                let name = std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned();
                let owner = glib::Type::from_glib(owner);
                if out.iter().any(|c| c.owner == owner && c.name == name) {
                    continue;
                }
                let parameter_type = if ptype.is_null() {
                    None
                } else {
                    Some(glib::VariantTy::from_ptr(ptype).as_str().to_owned())
                };
                let property = if prop.is_null() { None } else { Some(std::ffi::CStr::from_ptr(prop).to_string_lossy().into_owned()) };
                out.push(ClassAction { owner, name, parameter_type, property });
            }
            glib::gobject_ffi::g_type_class_unref(class as *mut _);
        }
        ty = t.parent();
    }
    out
}

impl ClassAction {
    pub fn describe(&self, agent: &Agent, w: &gtk::Widget) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("enabled".into(), Value::Null); // no public getter for class actions
        m.insert("parameter_type".into(), json!(self.parameter_type));
        m.insert("state_type".into(), Value::Null);
        m.insert("state".into(), Value::Null);
        if let Some(p) = &self.property {
            m.insert("property".into(), json!(p));
            if let Some(ps) = w.find_property(p) {
                m.insert("state_type".into(), json!(ps.value_type().name()));
                if ps.flags().contains(glib::ParamFlags::READABLE) {
                    let v = if secrecy::is_secret(w) && secrecy::redact_property(&ps) {
                        secrecy::redacted()
                    } else {
                        scalar_to_json(&ps, &w.property_value(ps.name())).unwrap_or(Value::Null)
                    };
                    m.insert("state".into(), v);
                }
            }
        }
        m.insert(
            "source".into(),
            json!({"kind": "widget_class", "owner": self.owner.name(), "ref": agent.registry.ref_for(w), "type": w.type_().name()}),
        );
        Value::Object(m)
    }
}

/// A class action named `full_name` reachable from the scope's widget chain.
pub fn find_class_action(scope: &Scope, full_name: &str) -> Option<(gtk::Widget, ClassAction)> {
    let widgets = match scope.kind {
        ScopeKind::App => return None,
        ScopeKind::Window => scope.widget.iter().cloned().collect(),
        ScopeKind::Widget => scope.chain(),
    };
    widgets.into_iter().find_map(|w| class_actions(&w).into_iter().find(|c| c.name == full_name).map(|c| (w, c)))
}

pub enum Lookup {
    /// The action is in an enumerable group.
    Group(GroupRef, String),
    Class(gtk::Widget, ClassAction),
    /// The group is enumerable and has no such action.
    Missing,
    /// The prefix is not enumerable from the scope: it may be an unregistered
    /// inserted group (activation by name can still work).
    Unresolved,
}

pub fn lookup(agent: &Agent, addr: &Addr) -> Result<Lookup, AgentError> {
    let full = addr.full_name();
    if let Some((w, c)) = find_class_action(&addr.scope, &full) {
        // An enumerable group's action of the same name wins over a class action.
        if find_group(agent, &addr.scope, &addr.prefix)?.is_none_or(|g| !g.group.has_action(&addr.action)) {
            return Ok(Lookup::Class(w, c));
        }
    }
    match find_group(agent, &addr.scope, &addr.prefix)? {
        Some(g) if g.group.has_action(&addr.action) => Ok(Lookup::Group(g, addr.action.clone())),
        Some(_) => Ok(Lookup::Missing),
        None => Ok(Lookup::Unresolved),
    }
}

// -- description ----------------------------------------------------------------

fn state_hint_json(hint: &glib::Variant) -> Value {
    match hint.classify() {
        glib::VariantClass::Tuple => json!({"range": variant_to_json(hint)}),
        glib::VariantClass::Array => json!({"options": variant_to_json(hint)}),
        _ => json!({"value": variant_to_json(hint)}),
    }
}

/// `{name, enabled, parameter_type, state_type, state, state_hint?}` of one
/// action of an enumerable group (`name` carries the prefix).
pub fn describe_group_action(group: &gio::ActionGroup, prefix: &str, action: &str, secret: bool) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(format!("{prefix}.{action}")));
    m.insert("enabled".into(), json!(group.is_action_enabled(action)));
    m.insert("parameter_type".into(), json!(group.action_parameter_type(action).map(|t| t.as_str().to_owned())));
    m.insert("state_type".into(), json!(group.action_state_type(action).map(|t| t.as_str().to_owned())));
    m.insert(
        "state".into(),
        match group.action_state(action) {
            Some(_) if secret => secrecy::redacted(),
            Some(s) => variant_to_json(&s),
            None => Value::Null,
        },
    );
    if let Some(h) = group.action_state_hint(action) {
        m.insert("state_hint".into(), if secret { secrecy::redacted() } else { state_hint_json(&h) });
    }
    Value::Object(m)
}

pub fn describe_group_entry(agent: &Agent, g: &GroupRef, action: &str, secret: bool) -> Value {
    let mut v = describe_group_action(&g.group, &g.prefix, action, secret);
    v["source"] = g.source(agent);
    v
}

// -- GVariant <-> JSON ----------------------------------------------------------

fn f64_json(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or_else(|| Value::String(x.to_string()))
}

/// JSON of a variant. Integers, booleans, doubles and strings map directly;
/// tuples and arrays to arrays (`a{s*}` to objects); `maybe` to the value or
/// `null`; `v` to `{"type","value"}`.
pub fn variant_to_json(v: &glib::Variant) -> Value {
    use glib::VariantClass as C;
    match v.classify() {
        C::Boolean => json!(v.get::<bool>().unwrap_or(false)),
        C::Byte => json!(v.get::<u8>().unwrap_or(0)),
        C::Int16 => json!(v.get::<i16>().unwrap_or(0)),
        C::Uint16 => json!(v.get::<u16>().unwrap_or(0)),
        C::Int32 => json!(v.get::<i32>().unwrap_or(0)),
        C::Uint32 => json!(v.get::<u32>().unwrap_or(0)),
        C::Int64 => json!(v.get::<i64>().unwrap_or(0)),
        C::Uint64 => json!(v.get::<u64>().unwrap_or(0)),
        C::Handle => {
            // SAFETY: the variant is of class handle.
            use glib::translate::ToGlibPtr;
            json!(unsafe { glib::ffi::g_variant_get_handle(v.to_glib_none().0) })
        }
        C::Double => f64_json(v.get::<f64>().unwrap_or(0.0)),
        C::String | C::ObjectPath | C::Signature => json!(v.str().unwrap_or_default()),
        C::Variant => match v.as_variant() {
            Some(inner) => json!({"type": inner.type_().as_str(), "value": variant_to_json(&inner)}),
            None => Value::Null,
        },
        C::Maybe => v.as_maybe().map_or(Value::Null, |i| variant_to_json(&i)),
        C::Array => {
            let elem = v.type_().element();
            if elem.is_dict_entry() && elem.key().as_str() == "s" {
                let mut m = Map::new();
                for e in v.iter() {
                    let k = e.child_value(0);
                    m.insert(k.str().unwrap_or_default().to_owned(), variant_to_json(&e.child_value(1)));
                }
                Value::Object(m)
            } else {
                Value::Array(v.iter().map(|c| variant_to_json(&c)).collect())
            }
        }
        C::Tuple | C::DictEntry => Value::Array(v.iter().map(|c| variant_to_json(&c)).collect()),
        _ => Value::String(v.print(true).to_string()),
    }
}

fn int_of(v: &Value) -> Option<i128> {
    let Value::Number(n) = v else { return None };
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
        .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e18).map(|f| f as i128))
}

/// Convert JSON to a variant of exactly `ty`. Messages never echo the value
/// (it may be secret), only its JSON kind.
pub fn json_to_variant(ty: &glib::VariantTy, v: &Value) -> Result<glib::Variant, String> {
    let ts = ty.as_str();
    let expected = |what: &str| format!("expected {what} for GVariant type '{ts}', got {}", kind(v));
    macro_rules! int {
        ($t:ty) => {{
            let n = int_of(v).ok_or_else(|| expected("an integer"))?;
            let n = <$t>::try_from(n).map_err(|_| format!("integer out of range for GVariant type '{ts}'"))?;
            Ok(n.to_variant())
        }};
    }
    // Non-string basic types and containers also accept GVariant text.
    if let Value::String(s) = v {
        if !matches!(ts, "s" | "o" | "g") && !ty.is_maybe() {
            return glib::Variant::parse(Some(ty), s).map_err(|_| format!("not valid GVariant text for type '{ts}'"));
        }
    }
    match ts {
        "b" => v.as_bool().map(|b| b.to_variant()).ok_or_else(|| expected("a boolean")),
        "y" => int!(u8),
        "n" => int!(i16),
        "q" => int!(u16),
        "i" => int!(i32),
        "u" => int!(u32),
        "x" => int!(i64),
        "t" => int!(u64),
        "h" => Err("handle (h) parameters cannot be passed through JSON".into()),
        "d" => v.as_f64().map(|f| f.to_variant()).ok_or_else(|| expected("a number")),
        "s" => v.as_str().map(|s| s.to_variant()).ok_or_else(|| expected("a string")),
        "o" => {
            let s = v.as_str().ok_or_else(|| expected("a string"))?;
            glib::variant::ObjectPath::try_from(s.to_owned()).map(|p| p.to_variant()).map_err(|_| "not a valid D-Bus object path".into())
        }
        "g" => {
            let s = v.as_str().ok_or_else(|| expected("a string"))?;
            glib::variant::Signature::try_from(s.to_owned()).map(|p| p.to_variant()).map_err(|_| "not a valid D-Bus signature".into())
        }
        "v" => {
            let m = v.as_object().ok_or_else(|| expected("an object {\"type\", \"value\"}"))?;
            let t = m.get("type").and_then(Value::as_str).ok_or_else(|| "a variant needs a string 'type'".to_string())?;
            let inner_ty = glib::VariantType::new(t).map_err(|_| "invalid inner GVariant type".to_string())?;
            let inner = json_to_variant(&inner_ty, m.get("value").unwrap_or(&Value::Null))?;
            Ok(glib::Variant::from_variant(&inner))
        }
        _ if ty.is_maybe() => match v {
            Value::Null => Ok(glib::Variant::from_none(ty.element())),
            other => Ok(glib::Variant::from_some(&json_to_variant(ty.element(), other)?)),
        },
        _ if ty.is_array() => {
            let elem = ty.element();
            if elem.is_dict_entry() && elem.key().as_str() == "s" && v.is_object() {
                let mut entries = Vec::new();
                for (k, val) in v.as_object().into_iter().flatten() {
                    entries.push(glib::Variant::from_dict_entry(&k.to_variant(), &json_to_variant(elem.value(), val)?));
                }
                Ok(glib::Variant::array_from_iter_with_type(elem, entries))
            } else {
                let items = v.as_array().ok_or_else(|| expected("an array"))?;
                let children: Result<Vec<_>, _> = items.iter().map(|i| json_to_variant(elem, i)).collect();
                Ok(glib::Variant::array_from_iter_with_type(elem, children?))
            }
        }
        _ if ty.is_tuple() => {
            let items = v.as_array().ok_or_else(|| expected("an array"))?;
            let tys: Vec<&glib::VariantTy> = ty.tuple_types().collect();
            if tys.len() != items.len() {
                return Err(format!("expected {} tuple elements for '{ts}', got {}", tys.len(), items.len()));
            }
            let children: Result<Vec<_>, _> = tys.iter().zip(items).map(|(t, i)| json_to_variant(t, i)).collect();
            Ok(glib::Variant::tuple_from_iter(children?))
        }
        _ if ty.is_dict_entry() => {
            let items = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| expected("a [key, value] array"))?;
            Ok(glib::Variant::from_dict_entry(&json_to_variant(ty.key(), &items[0])?, &json_to_variant(ty.value(), &items[1])?))
        }
        _ => Err(format!("GVariant type '{ts}' cannot be built from JSON")),
    }
}

/// Convert an optional parameter against an action's parameter type.
pub fn parameter_for(ptype: Option<&glib::VariantTy>, value: Option<&Value>) -> Result<Option<glib::Variant>, AgentError> {
    let invalid = |m: String| AgentError::new(ErrorCode::InvalidValue, m);
    match (ptype, value) {
        (None, None | Some(Value::Null)) => Ok(None),
        (None, Some(v)) => Err(invalid(format!("the action takes no parameter, got {}", kind(v)))),
        (Some(t), None | Some(Value::Null)) if !t.is_maybe() => {
            Err(invalid(format!("the action needs a parameter of GVariant type '{}'", t.as_str())))
        }
        (Some(t), v) => json_to_variant(t, v.unwrap_or(&Value::Null)).map(Some).map_err(invalid),
    }
}

/// Parameter for an action whose type is not known (unregistered group):
/// `null`/absent, or GVariant text (type inferred from the text).
pub fn parameter_untyped(value: Option<&Value>) -> Result<Option<glib::Variant>, AgentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => glib::Variant::parse(None, s)
            .map(Some)
            .map_err(|_| AgentError::new(ErrorCode::InvalidValue, "not valid GVariant text (the action's type is not known)")),
        Some(v) => Err(AgentError::new(
            ErrorCode::InvalidValue,
            format!(
                "the parameter type of this action is not known (unregistered group): pass GVariant text such as \"'abc'\" or \"uint32 5\", got {}",
                kind(v)
            ),
        )),
    }
}
