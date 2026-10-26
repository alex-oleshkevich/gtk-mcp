//! `measure_frames`: `GdkFrameClock` timings over a bounded window. Every
//! `after-paint` in the window is recorded with its frame counter and frame
//! time; presentation times come from the clock's timing history when the
//! compositor reported them. An idle surface paints nothing and the result
//! says so (`frames: 0`, `idle: true`) instead of inventing numbers.

use crate::agent::{Agent, Ctx};
use crate::selector;
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{
    AgentError, DEFAULT_FRAME_WINDOW_MS, ErrorCode, MAX_FRAME_WINDOW_MS, MeasureFramesParams, Target,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SLICE: Duration = Duration::from_millis(50);
const MAX_FRAMES_LISTED: usize = 120;
const MAX_LONG_FRAMES: usize = 10;
/// A frame interval above this many refresh intervals counts as missed.
const MISSED_FACTOR: f64 = 1.5;
const FALLBACK_REFRESH_US: i64 = 16_667;

struct Painted {
    counter: i64,
    frame_time: i64,
}

/// Disconnects the handler on drop (also when the request is cancelled).
struct Watch {
    clock: gdk::FrameClock,
    id: Option<glib::SignalHandlerId>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.clock.disconnect(id);
        }
    }
}

fn pick_widget(agent: &Agent, t: &Option<Target>) -> Result<gtk::Widget, AgentError> {
    match t {
        Some(t) => selector::resolve_widget(agent, t, true),
        None => {
            let win = agent
                .app
                .active_window()
                .or_else(|| selector::toplevels().into_iter().find(|w| w.is_mapped()))
                .or_else(|| selector::toplevels().into_iter().next())
                .ok_or_else(|| AgentError::new(ErrorCode::NotFound, "the app has no window"))?;
            Ok(win.upcast())
        }
    }
}

fn ms(us: f64) -> f64 {
    (us / 10.0).round() / 100.0 // microseconds to milliseconds, two decimals
}

fn stats(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| v[(((v.len() - 1) as f64) * p).round() as usize];
    json!({
        "min_ms": ms(v[0]), "avg_ms": ms(v.iter().sum::<f64>() / v.len() as f64),
        "p95_ms": ms(pct(0.95)), "p99_ms": ms(pct(0.99)), "max_ms": ms(v[v.len() - 1]),
    })
}

pub async fn measure_frames(agent: &Rc<Agent>, p: MeasureFramesParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let window_ms = p.duration_ms.unwrap_or(DEFAULT_FRAME_WINDOW_MS);
    if window_ms == 0 || window_ms > MAX_FRAME_WINDOW_MS {
        return Err(AgentError::invalid_params(format!("duration_ms must be in 1..={MAX_FRAME_WINDOW_MS}")));
    }
    let w = pick_widget(agent, &p.target)?;
    let native = w.native().ok_or_else(|| AgentError::new(ErrorCode::Detached, "the widget has no native surface"))?;
    let clock = w
        .frame_clock()
        .ok_or_else(|| AgentError::new(ErrorCode::TargetUnmapped, "the widget has no frame clock (not realized)"))?;

    let painted: Rc<RefCell<Vec<Painted>>> = Rc::default();
    let id = {
        let painted = painted.clone();
        clock.connect_after_paint(move |c| painted.borrow_mut().push(Painted { counter: c.frame_counter(), frame_time: c.frame_time() }))
    };
    let _watch = Watch { clock: clock.clone(), id: Some(id) };
    let start = Instant::now();
    let until = start + Duration::from_millis(window_ms);
    loop {
        ctx.check()?;
        let now = Instant::now();
        if now >= until {
            break;
        }
        glib::timeout_future((until - now).min(SLICE)).await;
    }
    let elapsed = start.elapsed();

    let frames = painted.borrow();
    let mut refresh_us: Option<i64> = None;
    let mut entries = Vec::new();
    let mut presented = Vec::new();
    for f in frames.iter() {
        let t = clock.timings(f.counter).filter(|t| t.is_complete());
        let presentation = t.as_ref().map(|t| t.presentation_time()).filter(|p| *p != 0);
        if let Some(r) = t.as_ref().map(|t| t.refresh_interval()).filter(|r| *r > 0) {
            refresh_us = Some(r);
        }
        presented.push(presentation);
        entries.push((f.counter, f.frame_time, presentation));
    }
    let refresh = refresh_us.unwrap_or(FALLBACK_REFRESH_US);
    let intervals: Vec<f64> = entries.windows(2).map(|w| (w[1].1 - w[0].1) as f64).collect();
    let pres_points: Vec<i64> = presented.iter().flatten().copied().collect();
    let pres_intervals: Vec<f64> = pres_points.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let latency: Vec<f64> = entries.iter().filter_map(|(_, ft, p)| p.map(|p| (p - ft) as f64)).collect();
    let missed = intervals.iter().filter(|i| **i > MISSED_FACTOR * refresh as f64).count();
    let skipped_counters: i64 = entries.windows(2).map(|w| (w[1].0 - w[0].0 - 1).max(0)).sum();

    let mut longest: Vec<(usize, f64)> = intervals.iter().copied().enumerate().collect();
    longest.sort_by(|a, b| b.1.total_cmp(&a.1));
    longest.truncate(MAX_LONG_FRAMES);
    let long_frames: Vec<Value> = longest
        .into_iter()
        .filter(|(_, i)| *i > MISSED_FACTOR * refresh as f64)
        .map(|(idx, i)| json!({"frame_counter": entries[idx + 1].0, "interval_ms": ms(i)}))
        .collect();

    let listed: Vec<Value> = entries
        .iter()
        .take(MAX_FRAMES_LISTED)
        .enumerate()
        .map(|(i, (counter, ft, pres))| {
            json!({
                "frame_counter": counter,
                "frame_time_us": ft,
                "interval_ms": if i == 0 { Value::Null } else { json!(ms(intervals[i - 1])) },
                "presentation_time_us": pres,
            })
        })
        .collect();

    let n = entries.len();
    let measured_s = elapsed.as_secs_f64().max(0.001);
    let target_ref = agent.registry.ref_for(&w);
    let native_ref = agent.registry.ref_for(native.upcast_ref::<glib::Object>());
    Ok(json!({
        "target": target_ref,
        "native": native_ref,
        "window_ms": elapsed.as_millis() as u64,
        "frames": n,
        "idle": n == 0,
        "fps": if n > 1 { json!(((n - 1) as f64 / ((entries[n - 1].1 - entries[0].1) as f64 / 1e6)).max(0.0)) } else { Value::Null },
        "average_fps_over_window": (n as f64 / measured_s * 10.0).round() / 10.0,
        "refresh_interval_ms": ms(refresh as f64),
        "refresh_interval_reported": refresh_us.is_some(),
        "frame_intervals": stats(&intervals),
        "missed_frames": missed,
        "skipped_frame_counters": skipped_counters,
        "long_frames": long_frames,
        "presentation": {
            "available": !pres_points.is_empty(),
            "frames_with_timing": pres_points.len(),
            "intervals": stats(&pres_intervals),
            "frame_to_presentation": stats(&latency),
        },
        "frames_listed": listed.len(),
        "frames_truncated": n > listed.len(),
        "timings": listed,
    }))
}
