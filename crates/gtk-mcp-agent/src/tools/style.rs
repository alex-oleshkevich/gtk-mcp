//! `inspect_style`, `apply_css`, `remove_css`, `highlight` (tools.md
//! layout-style, plan AC-10).
//!
//! Temporary CSS lives in `GtkCssProvider`s added to the display at user
//! priority. Every provider is owned by the connection that created it and is
//! removed when that connection drops ([`release_conn`], called from the
//! agent's disconnect hook) or when the app shuts down ([`release_all`]).
//! `inspect_style` reports public style information only; computed CSS and
//! style provenance are not public API (plan section 11).

use crate::agent::{Agent, Ctx};
use crate::{selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{
    AgentError, ApplyCssParams, ErrorCode, HighlightParams, InspectStyleParams, RemoveCssParams,
};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The class `highlight` puts on a widget; listed in `widgets::AGENT_CSS_CLASSES`.
pub const HIGHLIGHT_CLASS: &str = "gtk-mcp-highlight";
const HIGHLIGHT_CSS: &str = ".gtk-mcp-highlight { outline: 3px solid #ff2bd6; outline-offset: -3px; }";
/// Fixed provider priority: user level, above theme and application CSS.
const PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER;
const MAX_CSS_BYTES: usize = 256 * 1024;
const MAX_PROVIDERS_PER_CONN: usize = 64;
const SETTLE_TIMEOUT: Duration = Duration::from_millis(2000);
const SLICE: Duration = Duration::from_millis(25);
const QUIET_PERIOD: Duration = Duration::from_millis(40);

struct CssEntry {
    handle: String,
    conn: u64,
    display: gdk::Display,
    provider: gtk::CssProvider,
    /// Set for `highlight` entries: the widget carrying [`HIGHLIGHT_CLASS`].
    highlight: Option<glib::WeakRef<gtk::Widget>>,
}

#[derive(Default)]
struct CssState {
    next: u64,
    generation: u64,
    entries: Vec<CssEntry>,
}

thread_local! {
    static CSS: RefCell<CssState> = RefCell::new(CssState::default());
}

/// Changes to display-wide providers invalidate even apparently clean render nodes.
pub fn generation() -> u64 {
    CSS.with(|c| c.borrow().generation)
}

fn style_changed() {
    CSS.with(|c| {
        let mut c = c.borrow_mut();
        c.generation = c.generation.wrapping_add(1);
    });
}

fn install(conn: u64, css: &str, highlight: Option<&gtk::Widget>) -> Result<Installed, AgentError> {
    if css.len() > MAX_CSS_BYTES {
        return Err(AgentError::new(
            ErrorCode::InvalidValue,
            format!("css is {} bytes; the maximum is {MAX_CSS_BYTES}", css.len()),
        ));
    }
    let display = gdk::Display::default().ok_or_else(|| AgentError::new(ErrorCode::Unavailable, "there is no default display"))?;
    if CSS.with(|c| c.borrow().entries.iter().filter(|e| e.conn == conn).count()) >= MAX_PROVIDERS_PER_CONN {
        return Err(AgentError::new(
            ErrorCode::InvalidValue,
            format!("this client already holds {MAX_PROVIDERS_PER_CONN} temporary CSS providers; remove some first"),
        ));
    }
    let provider = gtk::CssProvider::new();
    let diagnostics: Rc<RefCell<Vec<Value>>> = Rc::default();
    {
        let diagnostics = diagnostics.clone();
        provider.connect_parsing_error(move |_, section, error| {
            let (start, end) = (section.start_location(), section.end_location());
            // GTK counts lines and columns from 0; report them 1-based.
            diagnostics.borrow_mut().push(json!({
                "severity": if error.is::<gtk::CssParserWarning>() { "warning" } else { "error" },
                "message": error.message(),
                "start": {"line": start.lines() + 1, "column": start.line_chars() + 1},
                "end": {"line": end.lines() + 1, "column": end.line_chars() + 1},
            }));
        });
    }
    #[allow(deprecated)]
    provider.load_from_data(css);
    let diagnostics = diagnostics.take();
    if diagnostics.iter().any(|d| d["severity"] == "error") {
        return Ok(Installed { handle: None, diagnostics });
    }
    gtk::style_context_add_provider_for_display(&display, &provider, PRIORITY);
    style_changed();
    let handle = CSS.with(|c| {
        let mut c = c.borrow_mut();
        c.next += 1;
        let handle = format!("css-{}", c.next);
        c.entries.push(CssEntry {
            handle: handle.clone(),
            conn,
            display,
            provider,
            highlight: highlight.map(|w| w.downgrade()),
        });
        handle
    });
    Ok(Installed { handle: Some(handle), diagnostics })
}

struct Installed {
    handle: Option<String>,
    diagnostics: Vec<Value>,
}

fn provider_count() -> usize {
    CSS.with(|c| c.borrow().entries.len())
}

/// Take `entry` out of GTK: the provider leaves the display and, for a
/// highlight, the class leaves the widget unless another highlight holds it.
fn uninstall(entry: CssEntry) {
    gtk::style_context_remove_provider_for_display(&entry.display, &entry.provider);
    style_changed();
    let Some(w) = entry.highlight.and_then(|w| w.upgrade()) else { return };
    let still_held = CSS.with(|c| {
        c.borrow().entries.iter().any(|e| e.highlight.as_ref().and_then(|x| x.upgrade()).is_some_and(|x| x == w))
    });
    if !still_held {
        w.remove_css_class(HIGHLIGHT_CLASS);
    }
}

fn take_entries(pred: impl Fn(&CssEntry) -> bool) -> Vec<CssEntry> {
    CSS.with(|c| {
        let mut c = c.borrow_mut();
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut c.entries).into_iter().partition(|e| pred(e));
        c.entries = keep;
        gone
    })
}

/// Include nested native surfaces (notably popovers), not just windows.
fn mapped_natives() -> Vec<gtk::Widget> {
    fn collect(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        if w.is_mapped() && w.is::<gtk::Native>() {
            out.push(w.clone());
        }
        let mut child = w.first_child();
        while let Some(w) = child {
            child = w.next_sibling();
            collect(&w, out);
        }
    }
    let mut out = Vec::new();
    for win in selector::toplevels() {
        collect(win.upcast_ref(), &mut out);
    }
    out
}

fn request_frame(w: &gtk::Widget) {
    w.queue_resize();
    w.queue_draw();
    if let Some(surface) = w.native().and_then(|n| n.surface()) {
        surface.queue_render();
    }
}

/// Disconnect cleanup cannot await, but must still request the restyle.
fn request_frames() {
    for native in mapped_natives() {
        request_frame(&native);
    }
}

/// A connection dropped: its providers and highlights disappear.
pub fn release_conn(conn: u64) {
    let gone = take_entries(|e| e.conn == conn);
    let any = !gone.is_empty();
    for e in gone {
        uninstall(e);
    }
    if any {
        request_frames();
    }
}

/// App shutdown: nothing of this agent stays installed.
pub fn release_all() {
    for e in take_entries(|_| true) {
        uninstall(e);
    }
}

// -- waiting for the restyle to reach a frame ------------------------------------

pub(super) struct PaintWatch {
    clock: gdk::FrameClock,
    id: Option<glib::SignalHandlerId>,
    last_paint: Rc<Cell<Option<Instant>>>,
}

impl PaintWatch {
    pub(super) fn new(clock: gdk::FrameClock) -> Self {
        let last_paint = Rc::new(Cell::new(None));
        let painted = last_paint.clone();
        let before = clock.frame_counter();
        let id = clock.connect_after_paint(move |clock| {
            if clock.frame_counter() > before {
                painted.set(Some(Instant::now()));
            }
        });
        Self { clock, id: Some(id), last_paint }
    }

    fn is_quiet(&self, now: Instant) -> bool {
        self.last_paint.get().is_some_and(|painted| now.duration_since(painted) >= QUIET_PERIOD)
    }

    /// CSS transitions can keep painting after the first post-change frame.
    /// Only a quiet clock proves that the final appearance reached the node.
    pub(super) async fn wait_quiet(&self, ctx: &Ctx, until: Instant) -> Result<(), &'static str> {
        loop {
            if ctx.is_cancelled() {
                return Err("cancelled");
            }
            let now = Instant::now();
            if self.is_quiet(now) {
                return Ok(());
            }
            if now >= until {
                return Err(if self.last_paint.get().is_some() { "animating" } else { "no_frame" });
            }
            glib::timeout_future(SLICE.min(until - now)).await;
        }
    }
}

impl Drop for PaintWatch {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.clock.disconnect(id);
        }
    }
}

/// Request a post-change frame on every mapped native, including popovers.
/// A missing clock is not evidence that a mapped surface has settled.
pub async fn settle_style(ctx: &Ctx) -> Result<(), &'static str> {
    let natives = mapped_natives();
    let mut watches = Vec::with_capacity(natives.len());
    let mut missing_clock = false;
    for native in &natives {
        if let Some(clock) = native.frame_clock() {
            watches.push(PaintWatch::new(clock));
        } else {
            missing_clock = true;
        }
        request_frame(native);
    }
    if missing_clock {
        return Err("no_frame_clock");
    }
    if natives.is_empty() {
        return Ok(());
    }
    let mut budget = SETTLE_TIMEOUT;
    if let Some(rem) = ctx.remaining() {
        budget = budget.min(rem * 3 / 4);
    }
    let until = Instant::now() + budget;
    loop {
        if ctx.is_cancelled() {
            return Err("cancelled");
        }
        let now = Instant::now();
        if watches.iter().all(|watch| watch.is_quiet(now)) {
            // Let paintables pick up the final frame (they swap in an idle).
            glib::timeout_future(Duration::from_millis(0)).await;
            return Ok(());
        }
        if now >= until {
            return Err(if watches.iter().any(|watch| watch.last_paint.get().is_none()) { "no_frame" } else { "animating" });
        }
        glib::timeout_future(SLICE.min(until - now)).await;
    }
}

/// Request one frame for `w` and wait, bounded, for the next `after-paint` of
/// its frame clock. `true` when a frame was painted.
pub async fn wait_one_paint(w: &gtk::Widget, clock: gdk::FrameClock, ctx: &Ctx, timeout: Duration) -> bool {
    let watch = PaintWatch::new(clock);
    w.queue_draw();
    if let Some(surface) = w.native().and_then(|n| n.surface()) {
        surface.queue_render();
    }
    let until = Instant::now() + timeout;
    loop {
        if watch.last_paint.get().is_some() {
            glib::timeout_future(Duration::from_millis(0)).await;
            return true;
        }
        if ctx.is_cancelled() || Instant::now() >= until {
            return false;
        }
        glib::timeout_future(SLICE).await;
    }
}

// -- tools ---------------------------------------------------------------------

pub async fn apply_css(_agent: &Rc<Agent>, p: ApplyCssParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let installed = install(ctx.conn.id, &p.css, None)?;
    let Some(handle) = installed.handle else {
        return Ok(json!({
            "installed": false,
            "handle": null,
            "diagnostics": installed.diagnostics,
            "providers": provider_count(),
        }));
    };
    let settled = settle_style(ctx).await;
    Ok(json!({
        "installed": true,
        "handle": handle,
        "priority": PRIORITY,
        "diagnostics": installed.diagnostics,
        "providers": provider_count(),
        "settled": settled.is_ok(),
        "settled_reason": settled.err(),
    }))
}

pub async fn remove_css(p: RemoveCssParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let conn = ctx.conn.id;
    let known = CSS.with(|c| c.borrow().entries.iter().find(|e| e.handle == p.handle).map(|e| e.conn));
    match known {
        None => Err(AgentError::new(ErrorCode::NotFound, format!("no temporary CSS provider has the handle '{}'", p.handle))),
        Some(owner) if owner != conn => Err(AgentError::new(
            ErrorCode::NotFound,
            format!("the handle '{}' belongs to another client; a client removes only its own providers", p.handle),
        )),
        Some(_) => {
            for e in take_entries(|e| e.handle == p.handle) {
                uninstall(e);
            }
            let settled = settle_style(ctx).await;
            Ok(json!({"removed": true, "handle": p.handle, "providers": provider_count(), "settled": settled.is_ok(), "settled_reason": settled.err()}))
        }
    }
}

pub async fn highlight(agent: &Rc<Agent>, p: HighlightParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let conn = ctx.conn.id;
    // Resolve first: a failing target must not clear the previous highlight.
    let target = match &p.target {
        Some(t) => Some(selector::resolve_widget(agent, t, true)?),
        None => None,
    };
    let previous: Vec<CssEntry> = take_entries(|e| e.conn == conn && e.highlight.is_some());
    let cleared: Vec<String> = previous.iter().map(|e| e.handle.clone()).collect();
    for e in previous {
        uninstall(e);
    }
    let Some(w) = target else {
        let settled = settle_style(ctx).await;
        return Ok(json!({"highlighted": false, "cleared": cleared, "settled": settled.is_ok(), "settled_reason": settled.err()}));
    };
    // Install first: a failed install must not touch the class, which another
    // client's highlight of the same widget may hold.
    let installed = install(conn, HIGHLIGHT_CSS, Some(&w))?;
    let Some(handle) = installed.handle else {
        return Err(AgentError::new(ErrorCode::Unavailable, "the highlight stylesheet did not parse"));
    };
    w.add_css_class(HIGHLIGHT_CLASS);
    let settled = settle_style(ctx).await;
    Ok(json!({
        "highlighted": true,
        "target": agent.registry.ref_for(&w),
        "handle": handle,
        "cleared": cleared,
        "settled": settled.is_ok(),
        "settled_reason": settled.err(),
    }))
}

fn state_flag_names(w: &gtk::Widget) -> Vec<String> {
    w.state_flags().iter_names().map(|(n, _)| n.to_lowercase()).collect()
}

/// Widget nodes from the root down, `name.class#id`; GTK's internal child nodes
/// are not part of the public API and are not listed.
fn css_path(w: &gtk::Widget) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(w.clone());
    while let Some(c) = cur {
        let mut s = c.css_name().to_string();
        for class in widgets::css_classes(&c) {
            s.push('.');
            s.push_str(&class);
        }
        let name = c.widget_name();
        if !name.is_empty() && name != c.type_().name() {
            s.push('#');
            s.push_str(&name);
        }
        parts.push(s);
        cur = c.parent();
    }
    parts.reverse();
    parts.join(" > ")
}

pub fn inspect_style(agent: &Rc<Agent>, p: InspectStyleParams) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, false)?;
    let dir = match w.direction() {
        gtk::TextDirection::Rtl => "rtl",
        gtk::TextDirection::Ltr => "ltr",
        _ => "none",
    };
    Ok(json!({
        "ref": agent.registry.ref_for(&w),
        "type": widgets::type_name(&w),
        "state": crate::registry::widget_state(&w).as_str(),
        "css_name": w.css_name().to_string(),
        "css_classes": widgets::css_classes(&w),
        "state_flags": state_flag_names(&w),
        "css_path": css_path(&w),
        "color": w.color().to_str().to_string(),
        "font": w.pango_context().font_description().map(|f| f.to_str().to_string()),
        "direction": dir,
        "opacity": w.opacity(),
        "scale_factor": w.scale_factor(),
        "temporary_css_providers": provider_count(),
    }))
}
