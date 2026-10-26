//! `interact`: typed semantic operations with state read-back (tools.md,
//! plan AC-4). Every state change goes through public GTK API / properties;
//! the supported operations of a widget are exactly `widgets::capabilities`.

use super::observe::finish;
use crate::agent::{Agent, Ctx};
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, InteractOp, InteractParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Excerpt limit for text echoed back by `fill` (characters).
const TEXT_ECHO_LIMIT: usize = 4096;
const SETTLE_POLL: Duration = Duration::from_millis(10);
const MAP_WAIT: Duration = Duration::from_millis(500);
const MEDIA_WAIT: Duration = Duration::from_millis(500);

fn err(code: ErrorCode, msg: impl Into<String>) -> AgentError {
    AgentError::new(code, msg)
}

fn unsupported(msg: impl Into<String>) -> AgentError {
    err(ErrorCode::UnsupportedOperation, msg)
}

fn invalid(msg: impl Into<String>) -> AgentError {
    err(ErrorCode::InvalidValue, msg)
}

pub async fn interact(agent: &Rc<Agent>, p: InteractParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, true)?;
    let r = agent.registry.ref_for(&w);
    let op = p.op.name();
    let caps = widgets::capabilities(&w);
    if !caps.contains(&op) {
        return Err(unsupported(format!(
            "{} {r} does not support '{op}'; supported operations: [{}]",
            widgets::type_name(&w),
            caps.join(", ")
        ))
        .with_data(json!({"supported": caps, "type": widgets::type_name(&w)})));
    }
    if !w.is_sensitive() {
        return Err(unsupported(format!("{r} is insensitive")).with_data(json!({"reason": "insensitive"})));
    }
    let observed = apply(agent, &w, &p.op, ctx).await?;
    Ok(finish(agent, r, observed, Some(&w), p.observe.as_ref(), ctx).await)
}

async fn apply(agent: &Agent, w: &gtk::Widget, op: &InteractOp, ctx: &Ctx) -> Result<Value, AgentError> {
    match op {
        InteractOp::Activate => activate(w),
        InteractOp::Focus => focus(w),
        InteractOp::Fill { text } => fill(w, text),
        InteractOp::SetChecked { checked } => set_checked(w, *checked),
        InteractOp::SetActive { active } => set_active(w, *active),
        InteractOp::SetValue { value } => set_value(w, *value),
        InteractOp::SetExpanded { expanded } => set_expanded(w, *expanded),
        InteractOp::Select { index } => select(w, *index),
        InteractOp::SelectPage { name } => select_page(w, name),
        InteractOp::Popup => popup(agent, w, true, ctx).await,
        InteractOp::Popdown => popup(agent, w, false, ctx).await,
        InteractOp::ScrollTo => scroll_to(agent, w),
        InteractOp::ScrollBy { dx, dy } => scroll_by(agent, w, *dx, *dy),
        InteractOp::Close => close(w),
        InteractOp::Play | InteractOp::Pause | InteractOp::Seek { .. } => media(w, op, ctx).await,
    }
}

// -- activate / focus ---------------------------------------------------------

fn activate(w: &gtk::Widget) -> Result<Value, AgentError> {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        // `clicked` directly: `GtkWidget::activate` on a button defers the click
        // by a press animation.
        b.emit_clicked();
    } else if !w.activate() {
        // Entries emit `activate` without registering it as the widget's
        // default activation.
        if glib::subclass::SignalId::lookup("activate", w.type_()).is_none() {
            return Err(unsupported(format!("{} has no activate action", widgets::type_name(w))));
        }
        w.emit_by_name::<()>("activate", &[]);
    }
    let mut m = Map::new();
    m.insert("activated".into(), json!(true));
    for (k, v) in [
        ("active", widgets::is_active(w)),
        ("checked", widgets::is_checked(w)),
        ("expanded", widgets::is_expanded(w)),
        ("selected", widgets::is_selected(w)),
    ] {
        if let Some(v) = v {
            m.insert(k.into(), json!(v));
        }
    }
    Ok(Value::Object(m))
}

/// `true` when `w` is the focus widget of its root or contains it (an entry
/// hands focus to its inner `GtkText`).
fn focus_within(w: &gtk::Widget) -> bool {
    match w.root().and_then(|r| r.focus()) {
        Some(f) => &f == w || f.is_ancestor(w),
        None => false,
    }
}

fn focus(w: &gtk::Widget) -> Result<Value, AgentError> {
    if !w.grab_focus() && !focus_within(w) {
        return Err(unsupported(format!("{} refused the focus", widgets::type_name(w))));
    }
    Ok(json!({"has_focus": focus_within(w)}))
}

// -- text ----------------------------------------------------------------------

fn echo_text(w: &gtk::Widget, text: &str) -> Value {
    if secrecy::is_secret(w) {
        return json!({"text": secrecy::redacted()});
    }
    let length = text.chars().count();
    if length > TEXT_ECHO_LIMIT {
        json!({"text": text.chars().take(TEXT_ECHO_LIMIT).collect::<String>(), "length": length, "truncated": true})
    } else {
        json!({"text": text, "length": length})
    }
}

fn fill(w: &gtk::Widget, text: &str) -> Result<Value, AgentError> {
    if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
        if !tv.is_editable() {
            return Err(unsupported("the text view is not editable"));
        }
        let buf = tv.buffer();
        buf.set_text(text);
        let (s, e) = buf.bounds();
        let now = buf.slice(&s, &e, true).to_string();
        return Ok(echo_text(w, &now));
    }
    let ed = w
        .dynamic_cast_ref::<gtk::Editable>()
        .ok_or_else(|| unsupported(format!("{} is not editable text", widgets::type_name(w))))?;
    if !ed.is_editable() {
        return Err(unsupported("the field is not editable"));
    }
    ed.set_text(text);
    if let Some(spin) = w.downcast_ref::<gtk::SpinButton>() {
        spin.update();
    }
    Ok(echo_text(w, &ed.text()))
}

// -- toggles -------------------------------------------------------------------

fn set_checked(w: &gtk::Widget, checked: bool) -> Result<Value, AgentError> {
    let cb = w.downcast_ref::<gtk::CheckButton>().ok_or_else(|| unsupported("not a check button"))?;
    cb.set_active(checked);
    Ok(json!({"checked": cb.is_active()}))
}

fn set_active(w: &gtk::Widget, active: bool) -> Result<Value, AgentError> {
    if let Some(s) = w.downcast_ref::<gtk::Switch>() {
        s.set_active(active);
    } else if let Some(t) = w.downcast_ref::<gtk::ToggleButton>() {
        t.set_active(active);
    } else if secrecy::is_type_name(w, "AdwSwitchRow") {
        w.set_property("active", active);
    } else {
        return Err(unsupported("not a switch or toggle"));
    }
    Ok(json!({"active": widgets::is_active(w)}))
}

fn adjustment_of(w: &gtk::Widget) -> Option<gtk::Adjustment> {
    if let Some(r) = w.dynamic_cast_ref::<gtk::Range>() {
        return Some(r.adjustment());
    }
    if let Some(s) = w.downcast_ref::<gtk::SpinButton>() {
        return Some(s.adjustment());
    }
    if secrecy::is_type_name(w, "AdwSpinRow") {
        return Some(w.property::<gtk::Adjustment>("adjustment"));
    }
    None
}

fn set_value(w: &gtk::Widget, value: f64) -> Result<Value, AgentError> {
    let adj = adjustment_of(w).ok_or_else(|| unsupported("the widget has no adjustment"))?;
    let (min, max) = (adj.lower(), adj.upper() - adj.page_size());
    if !value.is_finite() || value < min || value > max {
        return Err(invalid(format!("value {value} is outside [{min}, {max}]")).with_data(json!({"min": min, "max": max})));
    }
    if let Some(r) = w.dynamic_cast_ref::<gtk::Range>() {
        r.set_value(value);
    } else if let Some(s) = w.downcast_ref::<gtk::SpinButton>() {
        s.set_value(value);
    } else {
        adj.set_value(value);
    }
    Ok(json!({"value": adj.value(), "min": min, "max": max}))
}

fn set_expanded(w: &gtk::Widget, expanded: bool) -> Result<Value, AgentError> {
    if let Some(e) = w.downcast_ref::<gtk::Expander>() {
        e.set_expanded(expanded);
    } else if secrecy::is_type_name(w, "AdwExpanderRow") {
        w.set_property("expanded", expanded);
    } else {
        return Err(unsupported("not an expander"));
    }
    Ok(json!({"expanded": widgets::is_expanded(w)}))
}

// -- selection ------------------------------------------------------------------

fn check_index(index: u32, n: u32) -> Result<(), AgentError> {
    if index >= n {
        return Err(invalid(format!("index {index} is out of range; the model has {n} items")).with_data(json!({"n_items": n})));
    }
    Ok(())
}

fn select(w: &gtk::Widget, index: u32) -> Result<Value, AgentError> {
    if let Some(dd) = w.downcast_ref::<gtk::DropDown>() {
        check_index(index, dd.model().map_or(0, |m| m.n_items()))?;
        dd.set_selected(index);
        return Ok(json!({"selected": selected_json(dd.selected())}));
    }
    if secrecy::is_type_name(w, "AdwComboRow") {
        let n = w.property::<Option<gtk::gio::ListModel>>("model").map_or(0, |m| m.n_items());
        check_index(index, n)?;
        w.set_property("selected", index);
        return Ok(json!({"selected": selected_json(w.property::<u32>("selected"))}));
    }
    if let Some(lb) = w.downcast_ref::<gtk::ListBox>() {
        if lb.selection_mode() == gtk::SelectionMode::None {
            return Err(unsupported("the list box has selection-mode 'none'"));
        }
        let mut n = 0u32;
        while lb.row_at_index(n as i32).is_some() {
            n += 1;
        }
        check_index(index, n)?;
        let row = lb.row_at_index(index as i32).expect("checked");
        lb.select_row(Some(&row));
        return Ok(json!({"selected": lb.selected_row().map(|r| r.index())}));
    }
    if let Some(model) = list_selection_model(w) {
        check_index(index, model.n_items())?;
        if model.is::<gtk::NoSelection>() {
            return Err(unsupported("the list uses GtkNoSelection"));
        }
        model.select_item(index, true);
        let scrolled = scroll_list_to(w, index);
        let sel = model.selection();
        return Ok(json!({
            "selected": if sel.is_empty() { Value::Null } else { json!(sel.minimum()) },
            "selected_count": sel.size(),
            "scrolled_into_view": scrolled,
        }));
    }
    Err(unsupported("the widget has no selectable items"))
}

fn selected_json(pos: u32) -> Value {
    if pos == gtk::INVALID_LIST_POSITION { Value::Null } else { json!(pos) }
}

fn list_selection_model(w: &gtk::Widget) -> Option<gtk::SelectionModel> {
    if let Some(v) = w.downcast_ref::<gtk::ListView>() {
        return v.model();
    }
    if let Some(v) = w.downcast_ref::<gtk::GridView>() {
        return v.model();
    }
    if let Some(v) = w.downcast_ref::<gtk::ColumnView>() {
        return v.model();
    }
    None
}

/// `gtk_{list,grid,column}_view_scroll_to` are GTK 4.12 API; the agent builds
/// against the 4.10 floor, so the symbol is resolved at run time.
fn scroll_list_to(w: &gtk::Widget, pos: u32) -> bool {
    let ptr = w.as_ptr() as *mut std::ffi::c_void;
    let lookup = |name: &std::ffi::CStr| {
        // SAFETY: plain lookup in the global namespace.
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
    };
    const FLAG_NONE: u32 = 0;
    if w.is::<gtk::ColumnView>() {
        let f = lookup(c"gtk_column_view_scroll_to");
        if f.is_null() {
            return false;
        }
        // SAFETY: void gtk_column_view_scroll_to (GtkColumnView*, guint, GtkColumnViewColumn*, GtkListScrollFlags, GtkScrollInfo*)
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, u32, *mut std::ffi::c_void, u32, *mut std::ffi::c_void) =
            unsafe { std::mem::transmute(f) };
        unsafe { f(ptr, pos, std::ptr::null_mut(), FLAG_NONE, std::ptr::null_mut()) };
        return true;
    }
    let name = if w.is::<gtk::GridView>() { c"gtk_grid_view_scroll_to" } else { c"gtk_list_view_scroll_to" };
    let f = lookup(name);
    if f.is_null() {
        return false;
    }
    // SAFETY: void gtk_{list,grid}_view_scroll_to (View*, guint, GtkListScrollFlags, GtkScrollInfo*)
    let f: unsafe extern "C" fn(*mut std::ffi::c_void, u32, u32, *mut std::ffi::c_void) = unsafe { std::mem::transmute(f) };
    unsafe { f(ptr, pos, FLAG_NONE, std::ptr::null_mut()) };
    true
}

struct PageInfo {
    name: String,
    title: Option<String>,
}

fn stack_pages(w: &gtk::Widget) -> Vec<PageInfo> {
    let pages = w.property::<gtk::SelectionModel>("pages");
    (0..pages.n_items())
        .filter_map(|i| pages.item(i))
        .filter_map(|p| {
            let name = p.property::<Option<String>>("name")?;
            Some(PageInfo { name, title: p.property::<Option<String>>("title") })
        })
        .collect()
}

fn select_page(w: &gtk::Widget, name: &str) -> Result<Value, AgentError> {
    if w.is::<gtk::Stack>() || secrecy::is_type_name(w, "AdwViewStack") {
        let pages = stack_pages(w);
        if !pages.iter().any(|p| p.name == name) {
            return Err(unknown_page(name, pages.iter().map(|p| json!({"name": p.name, "title": p.title})).collect()));
        }
        w.set_property("visible-child-name", name);
        return Ok(json!({"visible_page": w.property::<Option<String>>("visible-child-name")}));
    }
    if let Some(nb) = w.downcast_ref::<gtk::Notebook>() {
        let n = nb.n_pages();
        let labels: Vec<Option<String>> = (0..n)
            .map(|i| {
                let child = nb.nth_page(Some(i))?;
                nb.tab_label_text(&child).or_else(|| nb.menu_label_text(&child)).map(Into::into)
            })
            .collect();
        let by_label = labels.iter().position(|l| l.as_deref() == Some(name));
        let idx = by_label.or_else(|| name.parse::<usize>().ok().filter(|i| *i < n as usize));
        let Some(idx) = idx else {
            return Err(unknown_page(
                name,
                labels.iter().enumerate().map(|(i, l)| json!({"name": l.clone().unwrap_or_else(|| i.to_string()), "index": i})).collect(),
            ));
        };
        nb.set_current_page(Some(idx as u32));
        let cur = nb.current_page().map(|c| c as usize).unwrap_or(idx);
        let label = labels.get(cur).cloned().flatten().unwrap_or_else(|| cur.to_string());
        return Ok(json!({"visible_page": label, "index": cur}));
    }
    Err(unsupported("the widget has no pages"))
}

fn unknown_page(name: &str, pages: Vec<Value>) -> AgentError {
    invalid(format!("no page named '{name}'")).with_data(json!({"pages": pages}))
}

// -- popovers -------------------------------------------------------------------

fn popover_of(w: &gtk::Widget) -> Option<gtk::Popover> {
    if let Some(p) = w.downcast_ref::<gtk::Popover>() {
        return Some(p.clone());
    }
    if let Some(mb) = w.downcast_ref::<gtk::MenuButton>() {
        return mb.popover();
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if let Some(p) = child.downcast_ref::<gtk::Popover>() {
            return Some(p.clone());
        }
        c = child.next_sibling();
    }
    None
}

/// Poll `cond` on the main loop until true or `limit` passes. Called after the
/// operation was dispatched: cancellation only ends the wait (`false`), so the
/// result still reaches the broker, as `$/late_result` when it timed out.
async fn wait_until(ctx: &Ctx, limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + limit;
    while !cond() {
        if ctx.is_cancelled() || Instant::now() >= until {
            return false;
        }
        glib::timeout_future(SETTLE_POLL).await;
    }
    true
}

async fn popup(agent: &Agent, w: &gtk::Widget, open: bool, ctx: &Ctx) -> Result<Value, AgentError> {
    let po = if let Some(mb) = w.downcast_ref::<gtk::MenuButton>() {
        if open {
            mb.popup();
        } else {
            mb.popdown();
        }
        mb.popover()
    } else {
        let po = popover_of(w).ok_or_else(|| unsupported("the widget has no popover"))?;
        if open {
            po.popup();
        } else {
            po.popdown();
        }
        Some(po)
    };
    let Some(po) = po else {
        return Err(unsupported("the menu button has no popover or menu model"));
    };
    // The popover's surface is created / destroyed on a later main-loop turn.
    let mapped = wait_until(ctx, MAP_WAIT, || po.is_mapped() == open).await;
    let key = if w.is::<gtk::MenuButton>() { "popover_visible" } else { "open" };
    Ok(json!({key: po.is_visible(), "popover": agent.registry.ref_for(&po), "popover_mapped": po.is_mapped(), "settled": mapped}))
}

// -- scrolling ------------------------------------------------------------------

fn adj_json(a: &gtk::Adjustment) -> Value {
    json!({"value": a.value(), "lower": a.lower(), "upper": a.upper(), "page_size": a.page_size()})
}

fn sw_json(agent: &Agent, sw: &gtk::ScrolledWindow) -> Value {
    json!({
        "ref": agent.registry.ref_for(sw),
        "hadjustment": adj_json(&sw.hadjustment()),
        "vadjustment": adj_json(&sw.vadjustment()),
    })
}

/// Scrolled windows around `w`, nearest first, excluding the one that
/// scrolls `w` itself when `w` is a scrollable (list view, text view, ...).
fn scrolled_windows_around(w: &gtk::Widget) -> Vec<gtk::ScrolledWindow> {
    let mut skip = None;
    if w.is::<gtk::Scrollable>() {
        let mut p = w.parent();
        if p.as_ref().is_some_and(|p| p.is::<gtk::Viewport>()) {
            p = p.and_then(|p| p.parent());
        }
        skip = p.and_then(|p| p.downcast::<gtk::ScrolledWindow>().ok());
    }
    let mut out = Vec::new();
    let mut p = w.parent();
    while let Some(a) = p {
        if let Some(sw) = a.downcast_ref::<gtk::ScrolledWindow>() {
            if Some(sw) != skip.as_ref() {
                out.push(sw.clone());
            }
        }
        p = a.parent();
    }
    out
}

/// New adjustment value that brings `[pos, pos+size)` into the page.
fn reveal(a: &gtk::Adjustment, pos: f64, size: f64) -> f64 {
    let (v, page) = (a.value(), a.page_size());
    let target = if size >= page || pos < v {
        pos
    } else if pos + size > v + page {
        pos + size - page
    } else {
        v
    };
    target.clamp(a.lower(), (a.upper() - page).max(a.lower()))
}

fn scroll_to(agent: &Agent, w: &gtk::Widget) -> Result<Value, AgentError> {
    if let Some(model) = list_selection_model(w) {
        // A list view scrolls to its selected item (GTK exposes no public
        // focus-position getter); without a selection it is scrolled as a widget.
        let sel = model.selection();
        if !sel.is_empty() && scroll_list_to(w, sel.minimum()) {
            return Ok(json!({"scrolled_to_item": sel.minimum()}));
        }
    }
    if !w.is_mapped() {
        return Err(err(ErrorCode::TargetUnmapped, "the widget is not mapped; it cannot be scrolled into view"));
    }
    let chain = scrolled_windows_around(w);
    if chain.is_empty() {
        return Err(unsupported("the widget has no GtkScrolledWindow ancestor"));
    }
    // Moving an inner scrolled window shifts `w` inside every outer one; the
    // bounds measured now are corrected by the accumulated shift.
    let (mut shift_x, mut shift_y) = (0.0f64, 0.0f64);
    let mut report = Vec::new();
    for sw in &chain {
        let Some(content) = sw.child() else { continue };
        let Some(b) = w.compute_bounds(&content) else {
            return Err(err(ErrorCode::TargetUnmapped, "the widget has no position inside the scrolled window"));
        };
        let (ha, va) = (sw.hadjustment(), sw.vadjustment());
        let x = f64::from(b.x()) + ha.value() - shift_x;
        let y = f64::from(b.y()) + va.value() - shift_y;
        let (old_h, old_v) = (ha.value(), va.value());
        ha.set_value(reveal(&ha, x, f64::from(b.width())));
        va.set_value(reveal(&va, y, f64::from(b.height())));
        shift_x += ha.value() - old_h;
        shift_y += va.value() - old_v;
        report.push(sw_json(agent, sw));
    }
    Ok(json!({"scrolled_windows": report}))
}

fn scroll_by(agent: &Agent, w: &gtk::Widget, dx: f64, dy: f64) -> Result<Value, AgentError> {
    let sw = w.downcast_ref::<gtk::ScrolledWindow>().ok_or_else(|| unsupported("not a scrolled window"))?;
    if !dx.is_finite() || !dy.is_finite() {
        return Err(invalid("dx and dy must be finite"));
    }
    let mut clamped = false;
    for (a, d) in [(sw.hadjustment(), dx), (sw.vadjustment(), dy)] {
        let want = a.value() + d;
        let max = (a.upper() - a.page_size()).max(a.lower());
        let got = want.clamp(a.lower(), max);
        clamped |= (got - want).abs() > f64::EPSILON;
        a.set_value(got);
    }
    let mut v = sw_json(agent, sw);
    v["clamped"] = json!(clamped);
    Ok(v)
}

// -- window / media ----------------------------------------------------------------

fn close(w: &gtk::Widget) -> Result<Value, AgentError> {
    let win = w.downcast_ref::<gtk::Window>().ok_or_else(|| unsupported("not a window"))?;
    win.close();
    Ok(json!({"closed": !win.is_visible(), "visible": win.is_visible()}))
}

fn media_stream(w: &gtk::Widget) -> Option<gtk::MediaStream> {
    if let Some(v) = w.downcast_ref::<gtk::Video>() {
        return v.media_stream();
    }
    w.downcast_ref::<gtk::MediaControls>().and_then(|c| c.media_stream())
}

fn media_json(m: &gtk::MediaStream) -> Value {
    json!({
        "playing": m.is_playing(),
        "timestamp": m.timestamp(),
        "duration": if m.duration() > 0 { json!(m.duration()) } else { Value::Null },
        "ended": m.is_ended(),
        "seekable": m.is_seekable(),
    })
}

async fn media(w: &gtk::Widget, op: &InteractOp, ctx: &Ctx) -> Result<Value, AgentError> {
    let m = media_stream(w).ok_or_else(|| unsupported("the widget has no media stream"))?;
    if !m.is_prepared() {
        return Err(unsupported("the media stream is not prepared yet"));
    }
    if let Some(e) = m.error() {
        return Err(unsupported(format!("the media stream is in error state: {e}")));
    }
    match op {
        InteractOp::Play => {
            m.play();
            wait_until(ctx, MEDIA_WAIT, || m.is_playing() || m.is_ended()).await;
        }
        InteractOp::Pause => {
            m.pause();
            wait_until(ctx, MEDIA_WAIT, || !m.is_playing()).await;
        }
        InteractOp::Seek { microseconds } => {
            if !m.is_seekable() {
                return Err(unsupported("the media stream is not seekable"));
            }
            let dur = m.duration();
            if *microseconds < 0 || (dur > 0 && *microseconds > dur) {
                return Err(invalid(format!("position {microseconds} is outside [0, {dur}] microseconds")).with_data(json!({"duration": dur})));
            }
            m.seek(*microseconds);
            wait_until(ctx, MEDIA_WAIT, || !m.is_seeking()).await;
        }
        _ => unreachable!("media() is only called for play/pause/seek"),
    }
    Ok(media_json(&m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::Conn;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_cancelled_wait_after_dispatch_ends_without_an_error() {
        let ctx = Ctx {
            id: 1,
            conn: Conn::detached(1),
            cancelled: Arc::new(AtomicBool::new(true)),
            deadline: None,
            started: Instant::now(),
        };
        let t = Instant::now();
        let settled = glib::MainContext::default().block_on(wait_until(&ctx, Duration::from_secs(5), || false));
        assert!(!settled);
        assert!(t.elapsed() < Duration::from_secs(1), "cancellation ends the wait at once");
    }
}
