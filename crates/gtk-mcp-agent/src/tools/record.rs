//! `record` / `stop_recording` (plan 5.6): turn manual interaction into a
//! replayable `batch` script.
//!
//! `record` installs a capture-phase `GtkEventControllerLegacy` on every
//! toplevel and every popover (popovers are separate natives that the
//! toplevel's controller never sees; spike S4) and emission hooks on the
//! signals users cause. Emissions while the agent runs its own code are
//! ignored; emissions within [`HUMAN_WINDOW_MS`] after a real input event are
//! `human`, the rest `app`. `stop_recording` returns the human emissions as
//! batch steps with selectors (automation_id > buildable_id > type + text +
//! ancestor, `nth` when still ambiguous) and lists the `app` ones separately.
//! Text typed into secret fields becomes `{{secret:N}}`.

use crate::agent::{Agent, Ctx};
use crate::diag::agent_busy;
use crate::hooks::{self, ListenerId};
use crate::{actions, secrecy, selector, widgets};
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, HUMAN_WINDOW_MS, RecordParams, Selector, StopRecordingParams};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

const MAX_EVENTS: usize = 5000;
/// A `change-state` this soon after an `activate` of the same action is its consequence.
const DERIVED_MS: i64 = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Human,
    App,
}

struct Rec {
    t_ms: u64,
    origin: Origin,
    signal: &'static str,
    /// Selector of the emitting widget (or action scope) as JSON.
    target: Value,
    step: Value,
    /// Value shown for app emissions (secrets already replaced).
    value: Value,
}

struct Recorder {
    conn: u64,
    seq: u64,
    started: std::time::Instant,
    last_input_us: Cell<i64>,
    events: RefCell<Vec<Rec>>,
    dropped: Cell<u64>,
    ignored: Cell<u64>,
    installed: RefCell<Vec<(glib::WeakRef<gtk::Widget>, gtk::EventControllerLegacy)>>,
    hooks: RefCell<Vec<ListenerId>>,
    secrets: RefCell<Vec<(glib::WeakRef<glib::Object>, Value)>>,
    /// Last `activate` per action (`name`, monotonic µs), for `change-state` consequences.
    last_activate: RefCell<Option<(String, i64)>>,
}

thread_local! {
    static RECORDER: RefCell<Option<Rc<Recorder>>> = const { RefCell::new(None) };
    static SEQ: Cell<u64> = const { Cell::new(1) };
}

fn marker_quark() -> glib::Quark {
    glib::Quark::from_str("gtk-mcp-record-controller")
}

fn mono_us() -> i64 {
    glib::monotonic_time()
}

// -- selectors ----------------------------------------------------------------

fn sel_json(s: &Selector) -> Value {
    serde_json::to_value(s).expect("Selector serializes")
}

/// Selector that resolves to exactly `w`, or `None` when the widget cannot be
/// found by any selector (detached, hidden internals).
fn selector_for(agent: &Agent, w: &gtk::Widget) -> Option<Value> {
    let secret = secrecy::is_secret(w);
    let editable = w.dynamic_cast_ref::<gtk::Editable>().is_some() || w.is::<gtk::TextView>();
    let mut candidates: Vec<Selector> = Vec::new();
    if let Some(a) = widgets::automation_id(w) {
        candidates.push(Selector { automation_id: Some(a), ..Default::default() });
    }
    if let Some(b) = widgets::buildable_id(w) {
        candidates.push(Selector { buildable_id: Some(b), ..Default::default() });
    }
    // type + text + nearest identified ancestor
    let text = if secret || editable { None } else { widgets::selector_text(w) };
    let mut anc = None;
    let mut cur = w.parent();
    while let Some(p) = cur {
        if let Some(a) = widgets::automation_id(&p) {
            anc = Some(Selector { automation_id: Some(a), ..Default::default() });
            break;
        }
        if let Some(b) = widgets::buildable_id(&p) {
            anc = Some(Selector { buildable_id: Some(b), ..Default::default() });
            break;
        }
        cur = p.parent();
    }
    let base = Selector { type_name: Some(widgets::type_name(w)), text, ..Default::default() };
    if let Some(a) = anc.clone() {
        candidates.push(Selector { ancestor: Some(Box::new(a)), ..base.clone() });
    }
    candidates.push(base);
    for mut c in candidates {
        let Ok(hits) = selector::find_all(agent, &c) else { continue };
        let Some(idx) = hits.iter().position(|h| &h.widget == w) else { continue };
        if hits.len() > 1 {
            c.nth = Some(idx);
        }
        return Some(sel_json(&c));
    }
    None
}

// -- recording ------------------------------------------------------------------

impl Recorder {
    fn secret_placeholder(&self, obj: &glib::Object, selector: &Value) -> String {
        let mut s = self.secrets.borrow_mut();
        let n = match s.iter().position(|(w, _)| w.upgrade().as_ref() == Some(obj)) {
            Some(i) => i + 1,
            None => {
                s.push((obj.downgrade(), selector.clone()));
                s.len()
            }
        };
        format!("{{{{secret:{n}}}}}")
    }

    fn push(&self, rec: Rec) {
        let mut ev = self.events.borrow_mut();
        if ev.len() >= MAX_EVENTS {
            self.dropped.set(self.dropped.get() + 1);
        } else {
            ev.push(rec);
        }
    }

    fn origin(&self) -> Origin {
        if mono_us() - self.last_input_us.get() <= HUMAN_WINDOW_MS as i64 * 1000 { Origin::Human } else { Origin::App }
    }
}

fn is_input(t: gdk::EventType) -> bool {
    use gdk::EventType as T;
    matches!(
        t,
        T::KeyPress
            | T::KeyRelease
            | T::ButtonPress
            | T::ButtonRelease
            | T::Scroll
            | T::TouchBegin
            | T::TouchUpdate
            | T::TouchEnd
            | T::TouchpadSwipe
            | T::TouchpadPinch
            | T::TouchpadHold
            | T::PadButtonPress
            | T::PadButtonRelease
    )
}

fn install_controller(rec: &Rc<Recorder>, w: &gtk::Widget) {
    // SAFETY: the qdata holds a `u64` only ever set here.
    let seen: Option<u64> = unsafe { w.qdata::<u64>(marker_quark()).map(|p| *p.as_ref()) };
    if seen == Some(rec.seq) {
        return;
    }
    let c = gtk::EventControllerLegacy::new();
    c.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak: Weak<Recorder> = Rc::downgrade(rec);
    c.connect_event(move |_, ev| {
        if is_input(ev.event_type()) {
            if let Some(r) = weak.upgrade() {
                r.last_input_us.set(mono_us());
            }
        }
        glib::Propagation::Proceed
    });
    w.add_controller(c.clone());
    // SAFETY: see above.
    unsafe { w.set_qdata(marker_quark(), rec.seq) };
    rec.installed.borrow_mut().push((w.downgrade(), c));
}

fn install_tree(rec: &Rc<Recorder>, w: &gtk::Widget) {
    if w.is::<gtk::Popover>() {
        install_controller(rec, w);
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        c = child.next_sibling();
        install_tree(rec, &child);
    }
}

fn install_natives(rec: &Rc<Recorder>) {
    for win in selector::toplevels() {
        install_controller(rec, win.upcast_ref());
        install_tree(rec, win.upcast_ref());
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Clicked,
    ToggleButton,
    CheckButton,
    Switch,
    Changed,
    Expander,
    Range,
    Spin,
    ActionActivate,
    ActionChangeState,
}

impl Kind {
    fn signal(self) -> &'static str {
        match self {
            Self::Clicked => "GtkButton::clicked",
            Self::ToggleButton => "GtkToggleButton::toggled",
            Self::CheckButton => "GtkCheckButton::toggled",
            Self::Switch => "GtkSwitch::state-set",
            Self::Changed => "GtkEditable::changed",
            Self::Expander => "GtkExpander::activate",
            Self::Range => "GtkRange::value-changed",
            Self::Spin => "GtkSpinButton::value-changed",
            Self::ActionActivate => "GSimpleAction::activate",
            Self::ActionChangeState => "GSimpleAction::change-state",
        }
    }
}

fn interact_step(target: &Value, op: Value) -> Value {
    let mut args = Map::new();
    args.insert("target".into(), target.clone());
    if let Value::Object(o) = op {
        args.extend(o);
    }
    json!({"tool": "interact", "arguments": Value::Object(args)})
}

/// Which scope and prefixed name address `action`? `None` when it lives in a
/// group the agent cannot name.
fn action_address(agent: &Agent, action: &gio::SimpleAction) -> Option<(Option<Value>, String)> {
    let name = action.name().to_string();
    let same = |g: Option<gio::Action>| g.is_some_and(|a| a.upcast_ref::<glib::Object>() == action.upcast_ref::<glib::Object>());
    if same(agent.app.lookup_action(&name)) {
        return Some((None, format!("app.{name}")));
    }
    let windows: Vec<gtk::Window> = agent.app.windows();
    let owners: Vec<&gtk::Window> = windows
        .iter()
        .filter(|w| w.dynamic_cast_ref::<gio::ActionMap>().is_some_and(|m| same(m.lookup_action(&name))))
        .collect();
    let owner = owners.first()?;
    let scope = if windows.iter().filter(|w| w.is::<gtk::ApplicationWindow>()).count() > 1 {
        selector_for(agent, owner.upcast_ref())
    } else {
        None
    };
    Some((scope, format!("win.{name}")))
}

fn on_emission(rec: &Rc<Recorder>, kind: Kind, instance: &glib::Object, values: &[glib::Value]) {
    if agent_busy() {
        rec.ignored.set(rec.ignored.get() + 1);
        return;
    }
    let Some(agent) = Agent::current() else { return };
    let origin = rec.origin();
    let t_ms = rec.started.elapsed().as_millis() as u64;

    // Action emissions address an action, not a widget.
    if let Some(action) = instance.downcast_ref::<gio::SimpleAction>() {
        let Some((scope, name)) = action_address(&agent, action) else { return };
        let name_key = action.name().to_string();
        let now = mono_us();
        let mut args = Map::new();
        if let Some(s) = &scope {
            args.insert("scope".into(), s.clone());
        }
        args.insert("name".into(), json!(name));
        let param = values.get(1).and_then(|v| v.get::<Option<glib::Variant>>().ok().flatten());
        let stateful = action.state_type().is_some();
        let (tool, value) = match kind {
            Kind::ActionActivate => {
                // A boolean toggle without parameter changes its state itself: `change-state` records it.
                if stateful && action.parameter_type().is_none() && action.state_type().is_some_and(|t| t == glib::VariantTy::BOOLEAN) {
                    *rec.last_activate.borrow_mut() = Some((name_key, now));
                    return;
                }
                *rec.last_activate.borrow_mut() = Some((name_key, now));
                if let Some(p) = &param {
                    args.insert("parameter".into(), actions::variant_to_json(p));
                }
                ("activate_action", param.as_ref().map_or(Value::Null, actions::variant_to_json))
            }
            _ => {
                let derived = rec
                    .last_activate
                    .borrow()
                    .as_ref()
                    .is_some_and(|(n, t)| *n == name_key && now - *t <= DERIVED_MS * 1000);
                if derived {
                    return;
                }
                let v = param.as_ref().map_or(Value::Null, actions::variant_to_json);
                args.insert("value".into(), v.clone());
                ("change_action_state", v)
            }
        };
        rec.push(Rec {
            t_ms,
            origin,
            signal: kind.signal(),
            target: json!({"action": name}),
            step: json!({"tool": tool, "arguments": Value::Object(args)}),
            value,
        });
        return;
    }

    let Some(w) = instance.downcast_ref::<gtk::Widget>() else { return };
    let (op, value): (Value, Value) = match kind {
        Kind::Clicked => {
            if w.is::<gtk::ToggleButton>() {
                return; // `toggled` carries the state
            }
            (json!({"operation": "activate"}), Value::Null)
        }
        Kind::ToggleButton => {
            let Some(b) = w.downcast_ref::<gtk::ToggleButton>() else { return };
            // A menu button's internal toggle: the user opened or closed the popover.
            if let Some(mb) = w.parent().filter(|p| p.is::<gtk::MenuButton>()) {
                let Some(sel) = selector_for(&agent, &mb) else { return };
                let op = if b.is_active() { "popup" } else { "popdown" };
                rec.push(Rec { t_ms, origin, signal: kind.signal(), target: sel.clone(), step: interact_step(&sel, json!({"operation": op})), value: json!(b.is_active()) });
                return;
            }
            (json!({"operation": "set_active", "active": b.is_active()}), json!(b.is_active()))
        }
        Kind::CheckButton => {
            let Some(b) = w.downcast_ref::<gtk::CheckButton>() else { return };
            (json!({"operation": "set_checked", "checked": b.is_active()}), json!(b.is_active()))
        }
        Kind::Switch => {
            let on = values.get(1).and_then(|v| v.get::<bool>().ok()).unwrap_or_else(|| w.property("active"));
            (json!({"operation": "set_active", "active": on}), json!(on))
        }
        Kind::Changed => {
            let Some(ed) = w.dynamic_cast_ref::<gtk::Editable>() else { return };
            // The inner GtkText of an entry re-emits on its wrapper; record the wrapper only.
            let mut p = w.parent();
            while let Some(a) = p {
                if a.dynamic_cast_ref::<gtk::Editable>().and_then(|e| e.delegate()).is_some_and(|d| d.dynamic_cast_ref::<gtk::Widget>() == Some(w)) {
                    return;
                }
                p = a.parent();
            }
            let text = ed.text().to_string();
            if secrecy::is_secret(w) {
                let Some(sel) = selector_for(&agent, w) else { return };
                let ph = rec.secret_placeholder(w.upcast_ref(), &sel);
                (json!({"operation": "fill", "text": ph}), json!(ph))
            } else {
                (json!({"operation": "fill", "text": text}), json!(text))
            }
        }
        Kind::Expander => {
            let Some(e) = w.downcast_ref::<gtk::Expander>() else { return };
            // Hooks run before the class handler flips the state.
            let now = !e.is_expanded();
            (json!({"operation": "set_expanded", "expanded": now}), json!(now))
        }
        Kind::Range => {
            let Some(r) = w.dynamic_cast_ref::<gtk::Range>() else { return };
            (json!({"operation": "set_value", "value": r.value()}), json!(r.value()))
        }
        Kind::Spin => {
            let Some(s) = w.downcast_ref::<gtk::SpinButton>() else { return };
            (json!({"operation": "set_value", "value": s.value()}), json!(s.value()))
        }
        Kind::ActionActivate | Kind::ActionChangeState => return,
    };
    let Some(sel) = selector_for(&agent, w) else { return };
    rec.push(Rec { t_ms, origin, signal: kind.signal(), target: sel.clone(), step: interact_step(&sel, op), value });
}

pub fn record(agent: &Rc<Agent>, _p: RecordParams, ctx: &Ctx) -> Result<Value, AgentError> {
    if RECORDER.with(|r| r.borrow().is_some()) {
        return Err(AgentError::invalid_params("a recording is already running; call stop_recording first"));
    }
    let _ = agent;
    let seq = SEQ.with(|s| {
        let v = s.get();
        s.set(v + 1);
        v
    });
    let rec = Rc::new(Recorder {
        conn: ctx.conn.id,
        seq,
        started: std::time::Instant::now(),
        last_input_us: Cell::new(i64::MIN / 2),
        events: RefCell::default(),
        dropped: Cell::new(0),
        ignored: Cell::new(0),
        installed: RefCell::default(),
        hooks: RefCell::default(),
        secrets: RefCell::default(),
        last_activate: RefCell::default(),
    });
    install_natives(&rec);

    let table: [(Kind, glib::Type, &str); 10] = [
        (Kind::Clicked, gtk::Button::static_type(), "clicked"),
        (Kind::ToggleButton, gtk::ToggleButton::static_type(), "toggled"),
        (Kind::CheckButton, gtk::CheckButton::static_type(), "toggled"),
        (Kind::Switch, gtk::Switch::static_type(), "state-set"),
        (Kind::Changed, gtk::Editable::static_type(), "changed"),
        (Kind::Expander, gtk::Expander::static_type(), "activate"),
        (Kind::Range, gtk::Range::static_type(), "value-changed"),
        (Kind::Spin, gtk::SpinButton::static_type(), "value-changed"),
        (Kind::ActionActivate, gio::SimpleAction::static_type(), "activate"),
        (Kind::ActionChangeState, gio::SimpleAction::static_type(), "change-state"),
    ];
    let mut hooked = Vec::new();
    for (kind, ty, name) in table {
        let weak = Rc::downgrade(&rec);
        let listener: hooks::Listener = Rc::new(move |instance, values| {
            if let Some(r) = weak.upgrade() {
                on_emission(&r, kind, instance, values);
            }
        });
        match hooks::add(ty, name, listener) {
            Some(id) => {
                rec.hooks.borrow_mut().push(id);
                hooked.push(kind.signal());
            }
            None => {
                teardown(&rec);
                return Err(AgentError::new(ErrorCode::Unavailable, format!("cannot hook {}", kind.signal())));
            }
        }
    }
    // Windows and popovers created later get their controller when they are mapped.
    {
        let weak = Rc::downgrade(&rec);
        let l: hooks::Listener = Rc::new(move |instance, _| {
            let Some(r) = weak.upgrade() else { return };
            if let Some(w) = instance.downcast_ref::<gtk::Widget>() {
                if w.is::<gtk::Popover>() || w.is::<gtk::Window>() {
                    install_controller(&r, w);
                    install_tree(&r, w);
                }
            }
        });
        if let Some(id) = hooks::add(gtk::Widget::static_type(), "map", l) {
            rec.hooks.borrow_mut().push(id);
        }
    }
    let controllers = rec.installed.borrow().len();
    RECORDER.with(|r| *r.borrow_mut() = Some(rec));
    Ok(json!({"recording": true, "hooks": hooked, "controllers": controllers, "human_window_ms": HUMAN_WINDOW_MS}))
}

fn teardown(rec: &Rc<Recorder>) {
    for id in rec.hooks.borrow_mut().drain(..) {
        hooks::remove(id);
    }
    for (weak, c) in rec.installed.borrow_mut().drain(..) {
        if let Some(w) = weak.upgrade() {
            w.remove_controller(&c);
            // SAFETY: only this module writes the marker, always as `u64`.
            unsafe {
                let _ = w.steal_qdata::<u64>(marker_quark());
            }
        }
    }
}

/// A connection dropped: its recording is discarded.
pub fn release_conn(conn: u64) {
    let rec = RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        if r.as_ref().is_some_and(|x| x.conn == conn) { r.take() } else { None }
    });
    if let Some(rec) = rec {
        teardown(&rec);
    }
}

pub fn release_all() {
    if let Some(rec) = RECORDER.with(|r| r.borrow_mut().take()) {
        teardown(&rec);
    }
}

/// Keep only the last of consecutive steps that set the same thing on the same widget.
fn coalesce(steps: Vec<(Value, u64)>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (s, _) in steps {
        let a = &s["arguments"];
        let key = (s["tool"].clone(), a["target"].clone(), a["operation"].clone());
        let mergeable = matches!(a["operation"].as_str(), Some("fill" | "set_value" | "set_checked" | "set_active" | "set_expanded"));
        if mergeable {
            if let Some(last) = out.last() {
                let la = &last["arguments"];
                if (last["tool"].clone(), la["target"].clone(), la["operation"].clone()) == key {
                    out.pop();
                }
            }
        }
        out.push(s);
    }
    out
}

pub fn stop_recording(_agent: &Rc<Agent>, _p: StopRecordingParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let rec = RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        match r.as_ref() {
            Some(x) if x.conn == ctx.conn.id => r.take(),
            _ => None,
        }
    });
    let Some(rec) = rec else {
        return Err(AgentError::new(ErrorCode::NotFound, "no recording is running on this connection; call record first"));
    };
    teardown(&rec);
    let events = std::mem::take(&mut *rec.events.borrow_mut());
    let human: Vec<(Value, u64)> = events.iter().filter(|e| e.origin == Origin::Human).map(|e| (e.step.clone(), e.t_ms)).collect();
    let steps = coalesce(human);
    let app: Vec<Value> = events
        .iter()
        .filter(|e| e.origin == Origin::App)
        .map(|e| json!({"t_ms": e.t_ms, "signal": e.signal, "target": e.target, "value": e.value, "step": e.step}))
        .collect();
    let secrets: Vec<Value> = rec
        .secrets
        .borrow()
        .iter()
        .enumerate()
        .map(|(i, (_, sel))| json!({"placeholder": format!("{{{{secret:{}}}}}", i + 1), "target": sel}))
        .collect();
    Ok(json!({
        "batch": {"steps": steps, "stop_on_error": true},
        "steps": steps.len(),
        "app_emissions": app,
        "secrets": secrets,
        "human_emissions": events.iter().filter(|e| e.origin == Origin::Human).count(),
        "ignored_agent_emissions": rec.ignored.get(),
        "dropped_events": rec.dropped.get(),
        "duration_ms": rec.started.elapsed().as_millis() as u64,
        "note": "replace {{secret:N}} placeholders before replaying",
    }))
}
