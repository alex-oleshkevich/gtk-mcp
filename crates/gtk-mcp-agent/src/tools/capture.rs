//! `capture`: settled widget render to PNG (plan 5.3).
//!
//! Algorithm: resolve -> `detached` / `target_unmapped` / `redacted` checks ->
//! settle -> `GtkWidgetPaintable` snapshot -> the native's renderer
//! `render_texture` -> PNG.
//!
//! Clean render nodes normally need no new frame. Display-wide CSS changes can
//! leave an apparently clean node behind, so the first settled capture of each
//! native after a provider change requests a post-change frame and waits for
//! its clock to go quiet, including any theme transitions.
//! Dirty widgets also wait for the frame clock's next `after-paint`, bounded by
//! `settle_timeout_ms`. No frame in time: render the last frame an earlier
//! capture saw with
//! `stale: true, stale_reason: "no_frame"`, or fail `capture_unavailable` when
//! there is none. The last-frame fallback is never served when the target
//! subtree contains a secret `Entry`/`Text`: that frame may have been painted
//! while the field was peeking, even if it is no longer peeking now.
//! S2 showed `after-paint` fires for covered, unfocused and
//! off-workspace windows, and never for unmapped ones (`target_unmapped`), so
//! `no_frame` means a throttled or frozen frame clock, not occlusion.

use crate::agent::{Agent, Ctx};
use crate::registry::{ObjState, widget_state};
use crate::{secrecy, selector, widgets};
use base64::Engine;
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, graphene, gsk, pango};
use gtk_mcp_protocol::{
    AgentError, Annotation, CaptureParams, CaptureResponse, CaptureResult, DEFAULT_SETTLE_TIMEOUT_MS, ErrorCode,
    MAX_FRAME_BYTES, MAX_SETTLE_TIMEOUT_MS,
};
use serde_json::Value;
use std::rc::Rc;
use std::time::{Duration, Instant};

const MAX_ANNOTATIONS: usize = 60;
const MAX_SCALE: f64 = 8.0;
const MAX_SIDE_PX: f64 = 16384.0;
const MAX_PIXELS: f64 = 64.0 * 1024.0 * 1024.0;
const FRAME_CACHE: usize = 32;
const SLICE: Duration = Duration::from_millis(50);

pub async fn capture(agent: &Rc<Agent>, p: CaptureParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, true)?;
    let resp = capture_widget(
        agent,
        &w,
        p.settle.unwrap_or(true),
        p.settle_timeout_ms.unwrap_or(DEFAULT_SETTLE_TIMEOUT_MS),
        p.annotate.unwrap_or(false),
        p.scale.unwrap_or(1.0),
        ctx,
    )
    .await?;
    Ok(serde_json::to_value(resp).expect("CaptureResponse serializes"))
}

/// Disconnects the `after-paint` handler when dropped, so a cancelled request
/// leaves nothing behind.
struct PaintWatch {
    clock: gdk::FrameClock,
    id: Option<glib::SignalHandlerId>,
    rx: async_channel::Receiver<()>,
}

impl PaintWatch {
    fn new(clock: gdk::FrameClock) -> Self {
        let (tx, rx) = async_channel::bounded::<()>(1);
        let before = clock.frame_counter();
        let id = clock.connect_after_paint(move |clock| {
            if clock.frame_counter() > before {
                let _ = tx.try_send(());
            }
        });
        Self { clock, id: Some(id), rx }
    }
}

impl Drop for PaintWatch {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.clock.disconnect(id);
        }
    }
}

/// Wait for an `after-paint` until `until`, in slices so cancellation is seen.
/// `Ok(true)` when a frame was painted.
async fn wait_paint(watch: &PaintWatch, until: Instant, ctx: &Ctx) -> Result<bool, AgentError> {
    loop {
        ctx.check()?;
        if watch.rx.try_recv().is_ok() {
            return Ok(true);
        }
        let now = Instant::now();
        if now >= until {
            return Ok(false);
        }
        let slice = SLICE.min(until - now);
        if glib::future_with_timeout(slice, watch.rx.recv()).await.is_ok() {
            return Ok(true);
        }
    }
}

thread_local! {
    /// Paintables of recently captured widgets: they keep following the
    /// widget's paints, so they hold its last frame when a later request finds
    /// the widget dirty and gets no new frame.
    static FRAMES: std::cell::RefCell<Vec<(glib::WeakRef<gtk::Widget>, gtk::WidgetPaintable)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The last CSS generation captured freshly on each native surface.
    static STYLE_FRAMES: std::cell::RefCell<Vec<(glib::WeakRef<gtk::Native>, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn remember_frame(w: &gtk::Widget, p: &gtk::WidgetPaintable) {
    FRAMES.with(|f| {
        let mut f = f.borrow_mut();
        f.retain(|(wk, pa)| wk.upgrade().is_some_and(|x| &x != w) && pa.widget().is_some());
        f.push((w.downgrade(), p.clone()));
        if f.len() > FRAME_CACHE {
            f.remove(0);
        }
    });
}

/// The remembered paintable of `w`, if it still shows a frame.
fn last_frame(w: &gtk::Widget) -> Option<gtk::WidgetPaintable> {
    FRAMES.with(|f| {
        f.borrow()
            .iter()
            .find(|(wk, _)| wk.upgrade().is_some_and(|x| &x == w))
            .map(|(_, p)| p.clone())
            .filter(has_render_data)
    })
}

fn captured_style(native: &gtk::Native) -> u64 {
    STYLE_FRAMES.with(|f| {
        f.borrow().iter().find(|(wk, _)| wk.upgrade().as_ref() == Some(native)).map_or(0, |(_, generation)| *generation)
    })
}

fn remember_style(native: &gtk::Native, generation: u64) {
    STYLE_FRAMES.with(|f| {
        let mut f = f.borrow_mut();
        f.retain(|(wk, _)| wk.upgrade().is_some_and(|n| &n != native));
        if f.len() == FRAME_CACHE {
            f.remove(0);
        }
        f.push((native.downgrade(), generation));
    });
}

/// Does the subtree rooted at `w` (inclusive) contain a secret `Entry`/`Text`,
/// whatever its current visibility? Mirrors the widget test in
/// `secrecy::subtree_peeking`, without judging whether it is revealing now.
fn subtree_has_secret(w: &gtk::Widget) -> bool {
    if (w.is::<gtk::Entry>() || w.is::<gtk::Text>()) && secrecy::is_secret(w) {
        return true;
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if subtree_has_secret(&child) {
            return true;
        }
        c = child.next_sibling();
    }
    false
}

/// The frame the fallback may serve for `w`. Never the remembered frame when
/// the subtree contains a secret field: the remembered paintable follows the
/// widget's paints, so it may hold a frame painted while the field was
/// peeking, even though `check_target` (current visibility) passes now.
/// Callers turn `None` into `capture_unavailable`, i.e. a fresh frame is the
/// only acceptable image for such targets.
fn fallback_frame(w: &gtk::Widget) -> Option<gtk::WidgetPaintable> {
    if subtree_has_secret(w) {
        None
    } else {
        last_frame(w)
    }
}

/// Does GTK hold a render node for the paintable's widget right now?
fn has_render_data(paintable: &gtk::WidgetPaintable) -> bool {
    let s = gtk::Snapshot::new();
    paintable.snapshot(&s, 1.0, 1.0);
    s.to_node().is_some()
}

fn check_target(agent: &Agent, w: &gtk::Widget) -> Result<(), AgentError> {
    let r = agent.registry.ref_for(w);
    if widget_state(w) == ObjState::Detached {
        return Err(AgentError::new(ErrorCode::Detached, format!("{r} is alive but not in a window")));
    }
    if !w.is_mapped() {
        return Err(AgentError::new(ErrorCode::TargetUnmapped, format!("{r} is not mapped; there is nothing to capture")));
    }
    if secrecy::subtree_peeking(w) {
        return Err(AgentError::new(
            ErrorCode::Redacted,
            format!("{r} contains a secret field that is currently revealing its content (visibility=true)"),
        ));
    }
    Ok(())
}

/// Render `w` to PNG. See the module documentation for the algorithm.
pub async fn capture_widget(
    agent: &Rc<Agent>,
    w: &gtk::Widget,
    settle: bool,
    settle_timeout_ms: u64,
    annotate: bool,
    scale: f64,
    ctx: &Ctx,
) -> Result<CaptureResponse, AgentError> {
    if !scale.is_finite() || scale <= 0.0 || scale > MAX_SCALE {
        return Err(AgentError::new(ErrorCode::InvalidValue, format!("scale must be in (0, {MAX_SCALE}]")));
    }
    check_target(agent, w)?;
    let native = w
        .native()
        .ok_or_else(|| AgentError::new(ErrorCode::CaptureUnavailable, "the widget has no native surface"))?;
    let clock = w
        .frame_clock()
        .ok_or_else(|| AgentError::new(ErrorCode::CaptureUnavailable, "the widget has no frame clock (not realized)"))?;

    let style_generation = super::style::generation();
    let style_changed = captured_style(&native) != style_generation;
    // A clean render node is only trustworthy when no provider changed since
    // this native's last fresh capture.
    let mut paintable = gtk::WidgetPaintable::new(Some(w));
    let clean = has_render_data(&paintable);

    let mut stale = false;
    let mut stale_reason = None;
    let mut source = paintable.clone();
    if settle && clean && !style_changed {
        // An unchanged, clean native needs no extra compositor frame.
    } else if settle {
        // Connect before requesting a frame; reject after-paint for a frame
        // that was already in progress when the style changed.
        let watch = PaintWatch::new(clock.clone());
        let style_watch = style_changed.then(|| super::style::PaintWatch::new(clock.clone()));
        let surface = native.surface();
        let request_frame = || {
            if style_changed {
                native.upcast_ref::<gtk::Widget>().queue_resize();
                native.upcast_ref::<gtk::Widget>().queue_draw();
            }
            w.queue_draw();
            if let Some(s) = &surface {
                // `queue_draw` requests no render when the widget is already
                // marked dirty (e.g. a popover waiting for a layout).
                s.queue_render();
            }
        };
        request_frame();
        let mut budget = Duration::from_millis(settle_timeout_ms.min(MAX_SETTLE_TIMEOUT_MS));
        if let Some(rem) = ctx.remaining() {
            // Keep a quarter of the remaining deadline for rendering.
            budget = budget.min(rem * 3 / 4);
        }
        let until = Instant::now() + budget;
        loop {
            if !wait_paint(&watch, until, ctx).await? {
                stale = true;
                stale_reason = Some("no_frame".to_string());
                break;
            }
            if let Some(watch) = &style_watch {
                if let Err(reason) = watch.wait_quiet(ctx, until).await {
                    ctx.check()?;
                    stale = true;
                    stale_reason = Some(reason.to_string());
                    break;
                }
            }
            // Paintables update in a high-priority idle after the paint. A
            // new paintable also avoids trusting a pre-restyle cached image.
            glib::timeout_future(Duration::from_millis(0)).await;
            paintable = gtk::WidgetPaintable::new(Some(w));
            if has_render_data(&paintable) {
                source = paintable.clone();
                break;
            }
            request_frame();
        }
        if stale {
            // No final frame arrived within the budget. Preserve the reason,
            // distinguishing a stalled clock from one that is still animating.
            source = fallback_frame(w).ok_or_else(|| {
                AgentError::new(
                    ErrorCode::CaptureUnavailable,
                    "no settled frame was painted within the settle timeout and there is no earlier frame of this widget",
                )
                .with_data(serde_json::json!({"reason": stale_reason}))
            })?;
        }
        // The paintable swaps its image in a high-priority idle after the paint.
        glib::timeout_future(Duration::from_millis(0)).await;
    } else if clean {
        stale = true;
        stale_reason = Some("settle_disabled".to_string());
    } else {
        source = fallback_frame(w).ok_or_else(|| {
            AgentError::new(
                ErrorCode::CaptureUnavailable,
                "the widget has pending changes and no earlier frame; capture with settle=true",
            )
            .with_data(serde_json::json!({"reason": "no_frame"}))
        })?;
        stale = true;
        stale_reason = Some("settle_disabled".to_string());
    }
    ctx.check()?;
    // The widget may have been unmapped / replaced while we waited.
    check_target(agent, w)?;

    let (png, width, height, annotations) = render(agent, w, &source, &native, scale, annotate)?;
    remember_frame(w, &source);
    if settle && !stale {
        remember_style(&native, style_generation);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    if b64.len() + 4096 > MAX_FRAME_BYTES {
        return Err(AgentError::new(
            ErrorCode::InvalidValue,
            format!("the PNG ({} bytes) does not fit into one protocol frame; capture a smaller target or lower `scale`", png.len()),
        ));
    }
    Ok(CaptureResponse {
        result: CaptureResult {
            capture_id: format!("cap-{}", agent.next_capture_seq()),
            target: agent.registry.ref_for(w),
            width,
            height,
            scale,
            stale,
            stale_reason,
            annotations,
        },
        png_base64: b64,
    })
}

fn render(
    agent: &Agent,
    w: &gtk::Widget,
    paintable: &gtk::WidgetPaintable,
    native: &gtk::Native,
    scale: f64,
    annotate: bool,
) -> Result<(Vec<u8>, u32, u32, Vec<Annotation>), AgentError> {
    let unavailable = |m: &str| AgentError::new(ErrorCode::CaptureUnavailable, m.to_string());
    // The captured rectangle is the widget's own bounds as GTK reports them
    // (`gtk_widget_compute_bounds(w, w)`): the CSS margin box, which can reach
    // beyond `width() x height()` (e.g. an entry with margins). The paintable
    // maps exactly this rectangle onto the size it is asked to draw.
    let bounds = w
        .compute_bounds(w)
        .ok_or_else(|| unavailable("the widget has no bounds (it has never been allocated)"))?;
    let (bw, bh) = (f64::from(bounds.width()), f64::from(bounds.height()));
    if bw < 1.0 || bh < 1.0 {
        return Err(unavailable("the widget has no size yet (it has never been allocated or painted)"));
    }
    let renderer = native.renderer().ok_or_else(|| unavailable("the surface has no renderer (it has never painted)"))?;
    let pw = (bw * scale).round().max(1.0);
    let ph = (bh * scale).round().max(1.0);
    if pw > MAX_SIDE_PX || ph > MAX_SIDE_PX || pw * ph > MAX_PIXELS {
        return Err(AgentError::new(
            ErrorCode::InvalidValue,
            format!("{pw}x{ph} pixels is too large; lower `scale`"),
        ));
    }

    // The widget's own render data. Nothing appended means GTK has no render
    // node for it (never drawn, or skipped in the last frame).
    let content = gtk::Snapshot::new();
    paintable.snapshot(&content, bw, bh);
    let content = content.to_node().ok_or_else(|| {
        unavailable("GTK has no render data for the widget (it has not been drawn yet, or it was outside the visible area in the last frame)")
    })?;

    let snapshot = gtk::Snapshot::new();
    snapshot.scale(scale as f32, scale as f32);
    // Widgets draw translucent backgrounds over their parents; flatten onto the
    // window background so the PNG looks like what is on screen (and matches
    // the same region of a window capture) instead of carrying alpha.
    snapshot.append_color(&backdrop(w), &graphene::Rect::new(0.0, 0.0, bw as f32, bh as f32));
    snapshot.append_node(&content);

    let mut annotations = Vec::new();
    if annotate {
        // Annotation rectangles are relative to the target; the image starts
        // at the target's bounds origin.
        let (ox, oy) = (bounds.x(), bounds.y());
        let items: Vec<_> = collect_annotations(agent, w, &bounds)
            .into_iter()
            .map(|(r, rect)| (r, graphene::Rect::new(rect.x() - ox, rect.y() - oy, rect.width(), rect.height())))
            .collect();
        draw_annotations(&snapshot, w, &items);
        for (r, rect) in items {
            annotations.push(Annotation {
                r#ref: r,
                x: f64::from(rect.x()) * scale,
                y: f64::from(rect.y()) * scale,
                width: f64::from(rect.width()) * scale,
                height: f64::from(rect.height()) * scale,
            });
        }
    }

    let node = snapshot.to_node().ok_or_else(|| unavailable("nothing was drawn"))?;
    let viewport = graphene::Rect::new(0.0, 0.0, pw as f32, ph as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    let png = texture.save_to_png_bytes();
    Ok((png.to_vec(), texture.width() as u32, texture.height() as u32, annotations))
}

/// Visible (mapped) descendants of `w` with their bounds relative to `w`,
/// nearest to the top-left first, at most [`MAX_ANNOTATIONS`]. Popovers are
/// not part of the render; descendants of secret widgets are skipped.
fn collect_annotations(agent: &Agent, w: &gtk::Widget, area: &graphene::Rect) -> Vec<(String, graphene::Rect)> {
    struct Cand {
        interesting: bool,
        ord: usize,
        widget: gtk::Widget,
        rect: graphene::Rect,
    }
    fn walk(parent: &gtk::Widget, root: &gtk::Widget, clip: &graphene::Rect, out: &mut Vec<Cand>) {
        let mut c = parent.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            if !child.is_mapped() || child.is::<gtk::Popover>() {
                continue;
            }
            let Some(b) = child.compute_bounds(root) else { continue };
            let mut child_clip = clip.clone();
            let Some(vis) = b.intersection(clip) else { continue };
            if vis.width() >= 1.0 && vis.height() >= 1.0 {
                let ord = out.len();
                out.push(Cand { interesting: is_interesting(&child), ord, widget: child.clone(), rect: vis });
            }
            if child.is::<gtk::ScrolledWindow>() || child.overflow() == gtk::Overflow::Hidden {
                match b.intersection(&child_clip) {
                    Some(r) => child_clip = r,
                    None => continue,
                }
            }
            if !secrecy::is_secret(&child) {
                walk(&child, root, &child_clip, out);
            }
        }
    }
    let mut cands = Vec::new();
    if !secrecy::is_secret(w) {
        walk(w, w, area, &mut cands);
    }
    // Widgets an agent would target come first, so the cap does not fill up
    // with structural internals; within a class, nearest to the top-left.
    cands.sort_by(|a, b| {
        let da = a.rect.x().hypot(a.rect.y());
        let db = b.rect.x().hypot(b.rect.y());
        b.interesting.cmp(&a.interesting).then(da.total_cmp(&db)).then(a.ord.cmp(&b.ord))
    });
    cands.truncate(MAX_ANNOTATIONS);
    cands.into_iter().map(|c| (agent.registry.ref_for(&c.widget), c.rect)).collect()
}

/// The window background colour of the widget's theme (libadwaita, then
/// classic GTK names); opaque white when the theme defines neither.
#[allow(deprecated)]
fn backdrop(w: &gtk::Widget) -> gdk::RGBA {
    let ctx = w.style_context();
    ["window_bg_color", "theme_bg_color"]
        .iter()
        .find_map(|n| ctx.lookup_color(n))
        .map(|c| gdk::RGBA::new(c.red(), c.green(), c.blue(), 1.0))
        .unwrap_or_else(|| gdk::RGBA::new(1.0, 1.0, 1.0, 1.0))
}

/// Named, text-bearing or operable widgets (not plain structure).
fn is_interesting(w: &gtk::Widget) -> bool {
    widgets::automation_id(w).is_some()
        || widgets::selector_text(w).is_some()
        || widgets::capabilities(w).iter().any(|c| *c != "focus" && *c != "scroll_to")
}

fn draw_annotations(snapshot: &gtk::Snapshot, w: &gtk::Widget, items: &[(String, graphene::Rect)]) {
    const PALETTE: [(f32, f32, f32); 6] = [
        (0.90, 0.15, 0.15),
        (0.10, 0.45, 0.90),
        (0.10, 0.65, 0.25),
        (0.85, 0.50, 0.05),
        (0.65, 0.20, 0.80),
        (0.05, 0.60, 0.65),
    ];
    let font = pango::FontDescription::from_string("Sans Bold 7");
    for (i, (r, rect)) in items.iter().enumerate() {
        let (cr, cg, cb) = PALETTE[i % PALETTE.len()];
        let outline = gdk::RGBA::new(cr, cg, cb, 1.0);
        snapshot.append_border(&gsk::RoundedRect::from_rect(rect.clone(), 0.0), &[1.0; 4], &[outline, outline, outline, outline]);
        let layout = w.create_pango_layout(Some(r));
        layout.set_font_description(Some(&font));
        let (tw, th) = layout.pixel_size();
        snapshot.append_color(
            &gdk::RGBA::new(cr, cg, cb, 0.85),
            &graphene::Rect::new(rect.x(), rect.y(), tw as f32 + 2.0, th as f32),
        );
        snapshot.save();
        snapshot.translate(&graphene::Point::new(rect.x() + 1.0, rect.y()));
        snapshot.append_layout(&layout, &gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
        snapshot.restore();
    }
}
