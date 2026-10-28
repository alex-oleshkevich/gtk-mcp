//! Issue 29: distinguish paint callbacks from compositor presentations and
//! never expose a negative frame-to-presentation latency.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn animated_frames_report_presentation_rate_and_nonnegative_latency() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(fx.dir.path())).unwrap() };
    c.settle_tree().await;
    c.json("interact", json!({"target": {"automation_id": "animate-toggle"}, "operation": "set_active", "active": true})).await;
    // The diagnostics section starts below the viewport; tick callbacks for a
    // clipped drawing area do not necessarily submit any compositor frames.
    c.json("interact", json!({"target": {"automation_id": "animated-area"}, "operation": "scroll_to"})).await;
    let r = c.json("measure_frames", json!({"target": {"automation_id": "animated-area"}, "duration_ms": 1500})).await;
    for timing in r["timings"].as_array().unwrap() {
        eprintln!("raw frame timing: {timing}");
        assert!(timing["complete"].is_boolean(), "retained timing must survive history eviction: {timing}");
    }
    eprintln!("presentation summary: {}", r["presentation"]);
    eprintln!("rates: paint_fps={}, presented_fps={}, refresh_hz={}, refresh_unavailable_reason={}", r["paint_fps"], r["presented_fps"], r["refresh_hz"], r["refresh_unavailable_reason"]);

    assert_eq!(r["idle"], false, "{r}");
    assert!(r["frames"].as_u64().unwrap() >= 2, "the animation must paint repeatedly: {r}");
    assert!(r["paint_fps"].as_f64().expect("paint callback rate") > 0.0, "{r}");
    assert!(r["average_paint_fps_over_window"].as_f64().unwrap() > 0.0, "{r}");
    if let Some(presented_fps) = r["presented_fps"].as_f64() {
        assert!(presented_fps > 0.0, "{r}");
        if let Some(refresh_hz) = r["refresh_hz"].as_f64() {
            assert!(refresh_hz > 0.0, "{r}");
            assert!(presented_fps <= refresh_hz * 1.05, "presentations must not exceed reported refresh: {r}");
            assert_eq!(r["refresh_interval_reported"], true, "{r}");
            assert!(r["refresh_unavailable_reason"].is_null(), "{r}");
        } else {
            // Nested niri can supply host-driven presentation timestamps with
            // refresh_interval=0; its advertised mode is not a refresh bound.
            assert_eq!(r["refresh_interval_reported"], false, "{r}");
            assert_eq!(r["refresh_unavailable_reason"], "Complete GDK frame timings contain no positive refresh intervals.", "{r}");
            for timing in r["timings"].as_array().unwrap().iter().filter(|t| t["complete"] == true) {
                assert_eq!(timing["refresh_interval_us"], 0, "{timing}");
            }
        }
        assert_eq!(r["presentation"]["available"], true, "{r}");
        assert_eq!(r["presentation"]["status"], "available", "{r}");
        assert!(r["presentation"]["distinct_presentations"].as_u64().unwrap() >= 2, "{r}");
    } else {
        assert_eq!(r["presentation"]["available"], false, "{r}");
        assert_eq!(r["presentation"]["status"], "unavailable", "{r}");
        assert_eq!(r["presentation"]["reason"], "Complete GDK frame timings contain no positive presentation timestamps.", "{r}");
        assert_eq!(r["presentation"]["frames_with_timing"], 0, "{r}");
        assert!(r["presentation"]["complete_frames"].as_u64().unwrap() >= 2, "{r}");
        for timing in r["timings"].as_array().unwrap().iter().filter(|t| t["complete"] == true) {
            assert_eq!(timing["raw_presentation_time_us"], 0, "unavailability must be backed by complete raw timings: {timing}");
        }
    }
    assert!(r["presentation"]["latency_samples_discarded"].is_u64(), "{r}");
    assert!(!r["presentation"]["latency_note"].as_str().unwrap().is_empty(), "{r}");
    let latency = &r["presentation"]["frame_to_presentation"];
    if !latency.is_null() {
        for field in ["min_ms", "avg_ms", "p95_ms", "p99_ms", "max_ms"] {
            assert!(latency[field].as_f64().unwrap() >= 0.0, "negative {field}: {r}");
        }
    }
}
