//! Broker-side diagnostics against a scripted agent (no display needed):
//! `read_events` cursors, filters and drops; `read_logs` availability, parsing
//! and filters (`docs/plan.md` 8, `docs/tools.md` diagnostics).

mod common;

use common::*;
use gtk_mcp::{Broker, Policy};
use gtk_mcp_protocol::*;
use serde_json::{Value, json};
use std::sync::Arc;

fn diag_policy() -> Policy {
    Policy { toolsets: vec!["core".into(), "diagnostics".into()], ..Policy::default() }
}

fn setup(mock: impl FnOnce(&std::path::Path) -> MockAgent, ring: usize) -> (tempfile::TempDir, MockAgent, Arc<Broker>) {
    let dir = tempfile::tempdir().unwrap();
    let agent = mock(dir.path());
    let b = broker_with(dir.path(), diag_policy(), |c| c.ring_capacity = ring);
    (dir, agent, b)
}

/// A scripted request that emits `events` then answers.
fn emitting(events: Vec<Value>) -> Vec<Step> {
    let mut v = vec![Step::Started];
    v.extend(events.into_iter().map(Step::Event));
    v.push(Step::Respond(json!({})));
    v
}

async fn events(b: &Arc<Broker>, args: Value) -> Value {
    b.call_tool("read_events", args).await.unwrap().json_content().unwrap()
}

#[tokio::test]
async fn read_events_pages_by_cursor_and_filters() {
    let evs: Vec<Value> = (0..5)
        .map(|i| json!({"kind": if i % 2 == 0 { "notify" } else { "signal" }, "subscription": format!("sub{}", i % 2), "n": i}))
        .collect();
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).script("list_windows", emitting(evs)).start(), 100);
    b.call_tool("list_windows", json!({})).await.unwrap();
    // The event notifications race the response; wait for all five.
    for _ in 0..100 {
        if events(&b, json!({})).await["entries"].as_array().unwrap().len() == 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let all = events(&b, json!({})).await;
    assert_eq!(all["entries"].as_array().unwrap().len(), 5, "{all}");
    assert_eq!(all["next_cursor"], 5);
    assert_eq!(all["dropped"], 0);
    assert_eq!(all["entries"][0]["cursor"], 0);
    assert_eq!(all["entries"][0]["n"], 0);

    let p1 = events(&b, json!({"limit": 2})).await;
    assert_eq!(p1["entries"].as_array().unwrap().len(), 2);
    assert_eq!(p1["next_cursor"], 2);
    assert_eq!(p1["more"], true);
    let p2 = events(&b, json!({"since": p1["next_cursor"], "limit": 2})).await;
    assert_eq!(p2["entries"][0]["n"], 2);
    let p3 = events(&b, json!({"since": p2["next_cursor"], "limit": 2})).await;
    assert_eq!((p3["entries"].as_array().unwrap().len(), p3["more"].clone()), (1, json!(false)));
    assert!(events(&b, json!({"since": 5})).await["entries"].as_array().unwrap().is_empty());

    let sub0 = events(&b, json!({"subscription": "sub0"})).await;
    assert_eq!(sub0["entries"].as_array().unwrap().len(), 3);
    let signals = events(&b, json!({"kind": "signal"})).await;
    assert_eq!(signals["entries"].as_array().unwrap().len(), 2);
    assert_eq!(b.call_tool("read_events", json!({"limit": 0})).await.unwrap_err().code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn read_events_reports_drops_from_the_bounded_ring() {
    let evs: Vec<Value> = (0..10).map(|i| json!({"kind": "notify", "n": i})).collect();
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).script("list_windows", emitting(evs)).start(), 4);
    b.call_tool("list_windows", json!({})).await.unwrap();
    for _ in 0..100 {
        if events(&b, json!({})).await["next_cursor"] == 10 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = events(&b, json!({"since": 0})).await;
    assert_eq!(r["entries"].as_array().unwrap().len(), 4);
    assert_eq!(r["entries"][0]["n"], 6);
    assert_eq!(r["dropped"], 6, "{r}");
    assert_eq!(r["dropped_total"], 6);
}

#[tokio::test]
async fn read_events_includes_late_results() {
    let (_d, _m, b) = setup(
        |d| MockAgent::builder(d).script("list_windows", vec![Step::Started, Step::LateResult(json!({"x": 1})), Step::Respond(json!({}))]).start(),
        10,
    );
    b.call_tool("list_windows", json!({})).await.unwrap();
    for _ in 0..100 {
        if !events(&b, json!({"kind": "late_result"})).await["entries"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = events(&b, json!({"kind": "late_result"})).await;
    assert_eq!(r["entries"][0]["result"], json!({"x": 1}));
}

#[tokio::test]
async fn read_logs_is_unavailable_for_an_external_app_without_forwarding() {
    let (_d, _m, b) = setup(|d| MockAgent::start(d), 10);
    let e = b.call_tool("read_logs", json!({})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable, "{e}");
    assert_eq!(e.data.unwrap()["log_forwarding"], false);
}

#[tokio::test]
async fn read_logs_forwarded_records_filter_and_page() {
    let (_d, mock, b) = setup(|d| MockAgent::start(d), 50);
    b.call_tool("list_apps", json!({})).await.unwrap();
    let rec = |level: &str, msg: &str| json!({"source": "forwarded", "level": level, "domain": "App", "message": msg, "fields": {"K": "v"}, "time_ms": 1});
    mock.push(method::LOG, rec("debug", "quiet"));
    mock.push(method::LOG, rec("warning", "careful one"));
    mock.push(method::LOG, rec("critical", "boom"));
    for _ in 0..100 {
        if b.call_tool("read_logs", json!({})).await.is_ok_and(|o| o.json_content().unwrap()["entries"].as_array().unwrap().len() == 3) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let all = b.call_tool("read_logs", json!({})).await.unwrap().json_content().unwrap();
    assert_eq!(all["entries"].as_array().unwrap().len(), 3, "{all}");
    assert_eq!(all["entries"][1]["fields"]["K"], "v");
    let warn = b.call_tool("read_logs", json!({"min_level": "warning"})).await.unwrap().json_content().unwrap();
    assert_eq!(warn["entries"].as_array().unwrap().len(), 2);
    let has = b.call_tool("read_logs", json!({"contains": "boom"})).await.unwrap().json_content().unwrap();
    assert_eq!(has["entries"][0]["message"], "boom");
    let paged = b.call_tool("read_logs", json!({"limit": 1, "since": 1})).await.unwrap().json_content().unwrap();
    assert_eq!((paged["entries"][0]["cursor"].clone(), paged["next_cursor"].clone(), paged["more"].clone()), (json!(1), json!(2), json!(true)));
    assert_eq!(b.call_tool("read_logs", json!({"source": "nope"})).await.unwrap_err().code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn read_logs_capability_alone_makes_logs_available() {
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).ack(|a| a.capabilities.push(Capability::LogForwarding)).start(), 10);
    let r = b.call_tool("read_logs", json!({})).await.unwrap().json_content().unwrap();
    assert_eq!(r["entries"].as_array().unwrap().len(), 0);
    assert_eq!(r["sources"]["forwarded"], true);
}
