//! `measure_frames`: `GdkFrameClock` timings over a bounded window. Every
//! `after-paint` retains its frame counter, frame time, and shared timings so
//! GDK's bounded history cannot evict samples during longer measurements.
//! Compositor feedback may complete later; collection allows a bounded settling
//! period without extending the paint window. An idle surface reports
//! `frames: 0`, `idle: true` instead of inventing numbers.

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
const TIMING_SETTLE: Duration = Duration::from_millis(250);
const MAX_FRAMES_LISTED: usize = 120;
const MAX_LONG_FRAMES: usize = 10;
/// A frame interval above this many refresh intervals counts as missed.
const MISSED_FACTOR: f64 = 1.5;
const FALLBACK_REFRESH_US: i64 = 16_667;

struct Painted {
    counter: i64,
    frame_time: i64,
    // Keep the shared timing alive after GDK evicts it from its short history.
    // The compositor can finish updating it after after-paint has returned.
    timing: Option<gdk::FrameTimings>,
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

struct FrameMetrics {
    paint_fps: Option<f64>,
    presented_fps: Option<f64>,
    refresh_us: Option<i64>,
    paint_intervals: Vec<f64>,
    presentation_intervals: Vec<f64>,
    latency: Vec<f64>,
    frames_with_timing: usize,
    distinct_presentations: usize,
    latency_samples_discarded: usize,
}

fn rate(count: usize, elapsed_us: i64) -> Option<f64> {
    (count > 1 && elapsed_us > 0).then(|| (count - 1) as f64 * 1e6 / elapsed_us as f64)
}

/// Samples contain frame time, complete presentation time, and refresh interval.
/// GDK specifies both timestamps in g_get_monotonic_time()'s domain. Reject
/// latency samples that contradict that contract: presentation before the frame
/// or either timestamp outside the monotonic observation window. Presentation
/// intervals do not require the frame and presentation clocks to agree.
fn summarize(samples: &[(i64, Option<i64>, Option<i64>)], observed_until_us: i64) -> FrameMetrics {
    let paint_intervals = samples.windows(2).map(|w| (w[1].0 - w[0].0) as f64).collect();
    let paint_fps = samples.first().zip(samples.last()).and_then(|(first, last)| rate(samples.len(), last.0 - first.0));
    let refresh_us = samples.iter().rev().find_map(|s| s.2.filter(|r| *r > 0));
    let mut presentations = Vec::new();
    let mut latency = Vec::new();
    let mut latency_samples_discarded = 0;
    for &(frame_time, presentation, _) in samples {
        let Some(presentation) = presentation.filter(|p| *p > 0) else { continue };
        presentations.push(presentation);
        if frame_time > 0 && presentation >= frame_time && presentation <= observed_until_us {
            latency.push((presentation - frame_time) as f64);
        } else {
            latency_samples_discarded += 1;
        }
    }
    let frames_with_timing = presentations.len();
    presentations.sort_unstable();
    presentations.dedup();
    let distinct_presentations = presentations.len();
    let presented_fps = presentations.first().zip(presentations.last()).and_then(|(first, last)| rate(distinct_presentations, last - first));
    let presentation_intervals = presentations.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    FrameMetrics {
        paint_fps,
        presented_fps,
        refresh_us,
        paint_intervals,
        presentation_intervals,
        latency,
        frames_with_timing,
        distinct_presentations,
        latency_samples_discarded,
    }
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
        clock.connect_after_paint(move |c| {
            let counter = c.frame_counter();
            painted.borrow_mut().push(Painted { counter, frame_time: c.frame_time(), timing: c.timings(counter) });
        })
    };
    let watch = Watch { clock: clock.clone(), id: Some(id) };
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
    drop(watch);
    // Stop counting paints at the measurement boundary, but continue dispatching
    // compositor feedback for the final frames. Do not extend window_ms/FPS.
    let settle_until = Instant::now() + TIMING_SETTLE;
    while painted.borrow().iter().any(|f| f.timing.as_ref().is_some_and(|t| !t.is_complete())) {
        ctx.check()?;
        let now = Instant::now();
        if now >= settle_until {
            break;
        }
        glib::timeout_future((settle_until - now).min(SLICE)).await;
    }

    let frames = painted.borrow();
    let samples: Vec<_> = frames
        .iter()
        .map(|f| {
            let t = f.timing.as_ref().filter(|t| t.is_complete());
            (
                f.frame_time,
                t.as_ref().map(|t| t.presentation_time()).filter(|p| *p > 0),
                t.as_ref().map(|t| t.refresh_interval()).filter(|r| *r > 0),
            )
        })
        .collect();
    let metrics = summarize(&samples, glib::monotonic_time());
    let refresh = metrics.refresh_us.unwrap_or(FALLBACK_REFRESH_US);
    let intervals = &metrics.paint_intervals;
    let missed = intervals.iter().filter(|i| **i > MISSED_FACTOR * refresh as f64).count();
    let skipped_counters: i64 = frames.windows(2).map(|w| (w[1].counter - w[0].counter - 1).max(0)).sum();

    let mut longest: Vec<(usize, f64)> = intervals.iter().copied().enumerate().collect();
    longest.sort_by(|a, b| b.1.total_cmp(&a.1));
    longest.truncate(MAX_LONG_FRAMES);
    let long_frames: Vec<Value> = longest
        .into_iter()
        .filter(|(_, i)| *i > MISSED_FACTOR * refresh as f64)
        .map(|(idx, i)| json!({"frame_counter": frames[idx + 1].counter, "interval_ms": ms(i)}))
        .collect();

    let listed: Vec<Value> = samples
        .iter()
        .take(MAX_FRAMES_LISTED)
        .enumerate()
        .map(|(i, (ft, pres, _))| {
            json!({
                "frame_counter": frames[i].counter,
                "frame_time_us": ft,
                "interval_ms": if i == 0 { Value::Null } else { json!(ms(intervals[i - 1])) },
                "presentation_time_us": pres,
                "raw_presentation_time_us": frames[i].timing.as_ref().map(|t| t.presentation_time()),
                "refresh_interval_us": frames[i].timing.as_ref().map(|t| t.refresh_interval()),
                "complete": frames[i].timing.as_ref().map(|t| t.is_complete()),
            })
        })
        .collect();

    let n = samples.len();
    let complete_frames = frames.iter().filter(|f| f.timing.as_ref().is_some_and(|t| t.is_complete())).count();
    let presentation_reason = if metrics.frames_with_timing > 0 {
        None
    } else if complete_frames > 0 {
        Some("Complete GDK frame timings contain no positive presentation timestamps.")
    } else {
        Some("No complete GDK frame timings were available before the collection deadline.")
    };
    let measured_s = elapsed.as_secs_f64().max(0.001);
    let target_ref = agent.registry.ref_for(&w);
    let native_ref = agent.registry.ref_for(native.upcast_ref::<glib::Object>());
    Ok(json!({
        "target": target_ref,
        "native": native_ref,
        "window_ms": elapsed.as_millis() as u64,
        "frames": n,
        "idle": n == 0,
        "paint_fps": metrics.paint_fps,
        "presented_fps": metrics.presented_fps,
        "average_paint_fps_over_window": (n as f64 / measured_s * 10.0).round() / 10.0,
        "refresh_hz": metrics.refresh_us.map(|r| 1e6 / r as f64),
        "refresh_interval_ms": ms(refresh as f64),
        "refresh_interval_reported": metrics.refresh_us.is_some(),
        "refresh_unavailable_reason": if metrics.refresh_us.is_some() {
            None
        } else if complete_frames > 0 {
            Some("Complete GDK frame timings contain no positive refresh intervals.")
        } else {
            Some("No complete GDK frame timings were available before the collection deadline.")
        },
        "frame_intervals": stats(intervals),
        "missed_frames": missed,
        "skipped_frame_counters": skipped_counters,
        "long_frames": long_frames,
        "presentation": {
            "available": metrics.frames_with_timing > 0,
            "status": if metrics.frames_with_timing > 0 { "available" } else { "unavailable" },
            "reason": presentation_reason,
            "complete_frames": complete_frames,
            "frames_with_timing": metrics.frames_with_timing,
            "distinct_presentations": metrics.distinct_presentations,
            "intervals": stats(&metrics.presentation_intervals),
            "frame_to_presentation": stats(&metrics.latency),
            "latency_samples_discarded": metrics.latency_samples_discarded,
            "latency_note": "GDK timestamps must share the monotonic clock domain. Latency samples before frame_time or outside the monotonic observation window are discarded.",
        },
        "frames_listed": listed.len(),
        "frames_truncated": n > listed.len(),
        "timings": listed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seventy_paints_per_second_with_sixty_presentations_reports_both_rates() {
        let samples: Vec<_> = (0..=70)
            .map(|i| {
                let presentation_index = (i * 60 + 69) / 70;
                (1_000_000 + i * 1_000_000 / 70, Some(1_000_000 + presentation_index * 1_000_000 / 60), Some(16_667))
            })
            .collect();
        let metrics = summarize(&samples, 2_000_000);
        assert!((metrics.paint_fps.unwrap() - 70.0).abs() < 0.001);
        assert!((metrics.presented_fps.unwrap() - 60.0).abs() < 0.001);
        assert_eq!(metrics.frames_with_timing, 71);
        assert_eq!(metrics.distinct_presentations, 61);
        assert_eq!(metrics.refresh_us, Some(16_667));
    }

    #[test]
    fn duplicate_presentation_timestamps_do_not_count_as_presented_frames() {
        let samples = [
            (100, Some(200), None),
            (150, Some(200), None),
            (200, Some(400), None),
            (250, Some(200), None),
            (300, Some(600), None),
        ];
        let metrics = summarize(&samples, 1_000);
        assert_eq!(metrics.frames_with_timing, 5);
        assert_eq!(metrics.distinct_presentations, 3);
        assert_eq!(metrics.presentation_intervals, [200.0, 200.0]);
        assert_eq!(metrics.presented_fps, Some(5_000.0));
    }

    #[test]
    fn negative_and_out_of_clock_domain_latency_samples_are_discarded() {
        let samples = [
            (100, Some(200), None),
            (300, Some(200), None),
            (400, Some(9_000_000_000), None),
            (0, Some(200), None),
            (450, None, None),
            (460, Some(0), None),
            (470, Some(-1), None),
        ];
        let metrics = summarize(&samples, 500);
        assert_eq!(metrics.latency, [100.0]);
        assert_eq!(metrics.latency_samples_discarded, 3);
        assert_eq!(metrics.frames_with_timing, 4);
        assert_eq!(stats(&metrics.latency)["min_ms"], 0.1);
    }

    #[test]
    fn unavailable_or_identical_timestamps_do_not_invent_rates_or_refresh() {
        let metrics = summarize(&[], 1_000);
        assert_eq!(metrics.paint_fps, None);
        assert_eq!(metrics.presented_fps, None);
        assert_eq!(metrics.refresh_us, None);
        let metrics = summarize(&[(100, None, Some(0)), (100, Some(200), Some(-1)), (100, Some(200), None)], 1_000);
        assert_eq!(metrics.paint_fps, None);
        assert_eq!(metrics.presented_fps, None);
        assert_eq!(metrics.refresh_us, None);
        assert_eq!(metrics.latency_samples_discarded, 0);
    }
}
