mod common;

use common::*;
use gtk_mcp::{Broker, Policy, Progress, ProgressSink};
use gtk_mcp_protocol::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn setup(mock: impl FnOnce(&std::path::Path) -> MockAgent, policy: Policy, deadline_ms: u64) -> (tempfile::TempDir, MockAgent, Arc<Broker>) {
    let dir = tempfile::tempdir().unwrap();
    let agent = mock(dir.path());
    let b = broker_with(dir.path(), policy, |c| c.default_deadline_ms = deadline_ms);
    (dir, agent, b)
}

fn interact() -> Value {
    json!({"session":"s1","target":"w1","operation":"activate"})
}

#[tokio::test]
async fn dispatch_forwards_with_deadline_and_session() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).respond("list_windows", json!({"windows":[]})).start(), Policy::default(), 10_000);
    let out = b.call_tool("list_windows", json!({})).await.unwrap();
    assert_eq!(out.json_content().unwrap(), json!({"windows":[]}));
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].1, "list_windows");
    assert_eq!(reqs[0].2["_meta"]["deadline_ms"], 10_000);
    assert_eq!(reqs[0].2["session"], "s1");
}

#[tokio::test]
async fn dispatch_deadline_formulas() {
    let cap = |id: &str| json!({"capture_id":id,"target":"w1","width":1,"height":1,"scale":1.0,"stale":false,"annotations":[],"png_base64":""});
    let (_d, mock, b) = setup(
        |d| {
            MockAgent::builder(d)
                .respond("wait_for", json!({"satisfied":true}))
                .respond("capture", cap("cap-1"))
                .respond("batch", json!({"steps":[]}))
                .start()
        },
        Policy::default(),
        10_000,
    );
    b.call_tool("wait_for", json!({"condition":"exists","target":"w1","timeout_ms":100})).await.unwrap();
    b.call_tool("wait_for", json!({"condition":"exists","target":"w1"})).await.unwrap();
    b.call_tool("capture", json!({"target":"w1","settle_timeout_ms":250})).await.unwrap();
    b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    // A huge settle bound neither overflows nor reaches the agent unclamped.
    b.call_tool("capture", json!({"target":"w1","settle_timeout_ms":u64::MAX})).await.unwrap();
    b.call_tool("batch", json!({"steps":[{"tool":"list_windows","arguments":{}}]})).await.unwrap();
    let d: Vec<u64> = mock.requests().iter().map(|r| r.2["_meta"]["deadline_ms"].as_u64().unwrap()).collect();
    // capture: settle bound (default DEFAULT_SETTLE_TIMEOUT_MS, at most MAX_SETTLE_TIMEOUT_MS) + 5 s
    assert_eq!(d, vec![2100, 7000, 5250, DEFAULT_SETTLE_TIMEOUT_MS + 5000, MAX_SETTLE_TIMEOUT_MS + 5000, 60_000]);
}

#[tokio::test]
async fn dispatch_unresponsive_when_never_started() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).script("list_windows", vec![]).start(), Policy::default(), 300);
    let e = b.call_tool("list_windows", json!({"session":"s1"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::TargetUnresponsive);
    assert_eq!(e.data.unwrap(), json!({"started": false}));
    let (id, ..) = mock.requests()[0].clone();
    assert_eq!(mock.wait_cancel(Duration::from_secs(2)), Some(id), "broker sends $/cancel");
    assert_eq!(mock.requests().len(), 1, "never retried");
}

#[tokio::test]
async fn dispatch_timeout_when_started() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).script("list_windows", vec![Step::Started]).start(), Policy::default(), 300);
    let e = b.call_tool("list_windows", json!({"session":"s1"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    assert_eq!(e.data.unwrap(), json!({"started": true}));
    assert!(mock.wait_cancel(Duration::from_secs(2)).is_some());
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn dispatch_response_after_deadline_is_ignored() {
    let (_d, mock, b) = setup(
        |d| {
            MockAgent::builder(d)
                .script("list_windows", vec![Step::Started, Step::Sleep(500), Step::Respond(json!({"stale":true}))])
                .respond("query", json!({"fresh":true}))
                .start()
        },
        Policy::default(),
        200,
    );
    assert!(b.call_tool("list_windows", json!({"session":"s1"})).await.is_err());
    tokio::time::sleep(Duration::from_millis(600)).await;
    let out = b.call_tool("query", json!({"session":"s1","selector":{}})).await.unwrap();
    assert_eq!(out.json_content().unwrap(), json!({"fresh":true}));
    assert_eq!(mock.requests().len(), 2);
    assert!(b.session("s1").unwrap().in_flight().is_empty());
}

#[tokio::test]
async fn dispatch_late_result_lands_in_event_buffer() {
    let (_d, mock, b) = setup(
        |d| {
            MockAgent::builder(d)
                
                .script("set_property", vec![Step::Started, Step::Sleep(500), Step::LateResult(json!({"observed":42}))])
                .start()
        },
        Policy::default(),
        200,
    );
    let e = b
        .call_tool("set_property", json!({"session":"s1","target":"w1","name":"x","value":42}))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    let (id, ..) = mock.requests()[0].clone();
    let s = b.session("s1").unwrap();
    assert!(wait_until(|| !s.events_since(0).entries.is_empty()));
    let r = s.events_since(0);
    assert_eq!(r.entries[0].value, json!({"kind":"late_result","id":id,"result":{"observed":42}}));
    assert_eq!(r.next_cursor, 1);
    assert!(s.events_since(1).entries.is_empty());
}

#[tokio::test]
async fn dispatch_event_ring_is_bounded_with_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::start(dir.path());
    let b = broker_with(dir.path(), Policy::default(), |c| c.ring_capacity = 3);
    b.call_tool("list_apps", json!({})).await.unwrap();
    for n in 0..5 {
        mock.push("$/event", json!({"n": n}));
    }
    let s = b.session("s1").unwrap();
    assert!(wait_until(|| s.events_since(0).next_cursor == 5));
    let r = s.events_since(0);
    assert_eq!(r.entries.iter().map(|e| e.cursor).collect::<Vec<_>>(), vec![2, 3, 4]);
    assert_eq!(r.dropped, 2);
    assert_eq!(s.events_since(3).dropped, 0);
    assert_eq!(s.events_since(3).entries.len(), 2);
}

#[tokio::test]
async fn dispatch_progress_maps_to_sink() {
    let (_d, _mock, b) = setup(
        |d| {
            MockAgent::builder(d)
                .script("wait_for", vec![Step::Started, Step::Progress(1.0, Some(3.0)), Step::Progress(2.0, Some(3.0)), Step::Sleep(50), Step::Respond(json!({"satisfied":true}))])
                .start()
        },
        Policy::default(),
        10_000,
    );
    let seen = Arc::new(Mutex::new(Vec::<Progress>::new()));
    let s2 = seen.clone();
    let sink: ProgressSink = Arc::new(move |p| s2.lock().unwrap().push(p));
    b.call_tool_with_progress("wait_for", json!({"condition":"exists","target":"w1"}), Some(sink)).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], Progress { progress: 2.0, total: Some(3.0), message: Some("working".into()) });
}

#[tokio::test]
async fn dispatch_many_requests_in_flight_on_one_connection() {
    let (_d, _mock, b) = setup(
        |d| {
            MockAgent::builder(d)
                .script("list_windows", vec![Step::Started, Step::Sleep(250), Step::Respond(json!({"n":"slow"}))])
                .respond("query", json!({"n":"fast"}))
                .start()
        },
        Policy::default(),
        10_000,
    );
    let slow = b.call_tool("list_windows", json!({"session":"s1"}));
    let fast = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let t = std::time::Instant::now();
        let r = b.call_tool("query", json!({"session":"s1","selector":{}})).await;
        (r, t.elapsed())
    };
    let (slow, (fast, took)) = tokio::join!(slow, fast);
    assert_eq!(slow.unwrap().json_content().unwrap()["n"], "slow");
    assert_eq!(fast.unwrap().json_content().unwrap()["n"], "fast");
    assert!(took < Duration::from_millis(200));
}

#[tokio::test]
async fn dispatch_agent_error_is_returned_verbatim() {
    let err = AgentError::new(ErrorCode::AmbiguousTarget, "two buttons").with_data(json!({"candidates":["w1","w2"]}));
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).script("inspect", vec![Step::Started, Step::Error(err.clone())]).start(), Policy::default(), 10_000);
    let e = b.call_tool("inspect", json!({"session":"s1","target":{"text":"Save"}})).await.unwrap_err();
    assert_eq!(e, err);
}

#[tokio::test]
async fn dispatch_bad_params_and_unknown_tools_send_nothing() {
    let (_d, mock, b) = setup(|d| MockAgent::start(d), Policy::default(), 10_000);
    b.call_tool("list_apps", json!({})).await.unwrap();
    for (tool, args) in [
        ("snapshot", json!({"session":"s1","max_depth":"deep"})),
        ("inspect", json!({"session":"s1"})),
        ("nope", json!({})),
        ("launch_app", json!({})),
        ("list_windows", json!([1])),
    ] {
        let e = b.call_tool(tool, args).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidParams, "{tool}");
    }
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn dispatch_excluded_tool_is_absent_from_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::start(dir.path());
    let policy = Policy { exclude_tools: vec!["capture".into()], ..Policy::default() };
    let b = broker_with(dir.path(), policy, |_| {});
    assert!(!b.enabled_tools().iter().any(|t| t.name == "capture"));
    assert_eq!(b.enabled_tools().len(), gtk_mcp_protocol::TOOLS.len() - 1);
    let e = b.call_tool("capture", json!({"session":"s1","target":"w1"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(mock.requests().is_empty());
    // a batch step naming it is rejected too
    let e = b
        .call_tool("batch", json!({"steps":[{"tool":"capture","arguments":{"target":"w1"}}]}))
        .await
        .unwrap_err();
    assert!(e.message.contains("batch step 1:"), "{}", e.message);
    assert_eq!(e.data, Some(json!({"step": 1})), "steps are numbered from 1, as in the agent and $N.ref");
}

#[tokio::test]
async fn dispatch_mutation_forwards_without_any_flag() {
    let (_d, mock, b) = setup(
        |d| MockAgent::builder(d).respond("interact", json!({"target":"w1","observed":{}})).start(),
        Policy::default(),
        10_000,
    );
    let out = b.call_tool("interact", interact()).await.unwrap();
    assert_eq!(out.json_content().unwrap()["target"], "w1");
    let reqs = mock.requests();
    assert_eq!(reqs[0].1, "interact");
    assert_eq!(reqs[0].2["operation"], "activate");
}

/// gm-n4j: a request nested deeper than the agent can parse fails at once
/// instead of reaching the agent as an unparsable frame and hanging until the
/// deadline.
#[tokio::test]
async fn dispatch_too_deeply_nested_request_fails_at_once() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).start(), Policy::default(), 30_000);
    let value = (0..MAX_FRAME_NESTING).fold(json!(1), |v, _| json!([v]));
    let t = std::time::Instant::now();
    let e = b.call_tool("set_property", json!({"session":"s1","target":"w1","name":"x","value":value})).await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(5), "answered without waiting for the deadline");
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(e.message.contains("levels deep"), "{}", e.message);
    assert!(mock.requests().is_empty());
    b.call_tool("list_windows", json!({"session":"s1"})).await.unwrap();
}

// -- batch ------------------------------------------------------------------

#[tokio::test]
async fn batch_read_only_runs() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).respond("batch", json!({"steps":[]})).start(), Policy::default(), 10_000);
    let steps = json!([
        {"tool":"query","arguments":{"selector":{"text":"Save"}}},
        {"tool":"list_windows","arguments":{}},
        {"tool":"wait_for","arguments":{"condition":"exists","target":"$1.ref"}},
    ]);
    b.call_tool("batch", json!({"session":"s1","steps":steps})).await.unwrap();
    let reqs = mock.requests();
    assert_eq!(reqs[0].1, "batch");
    assert_eq!(reqs[0].2["steps"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn batch_with_mutating_step_forwards_without_any_flag() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).respond("batch", json!({"steps":[]})).start(), Policy::default(), 10_000);
    let steps = json!([{"tool":"list_windows","arguments":{}},{"tool":"interact","arguments":{"target":"w1","operation":"activate"}}]);
    b.call_tool("batch", json!({"session":"s1","steps":steps})).await.unwrap();
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].1, "batch");
}

#[tokio::test]
async fn batch_invalid_step_sends_nothing() {
    let (_d, mock, b) = setup(|d| MockAgent::builder(d).start(), Policy::default(), 10_000);
    b.call_tool("list_apps", json!({})).await.unwrap();
    let bad_cases = vec![
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"no_such_tool","arguments":{}}]),
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"inspect","arguments":{}}]),
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"batch","arguments":{"steps":[]}}]),
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"list_apps","arguments":{}}]),
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"diff_snapshots","arguments":{"from":"a","to":"b"}}]),
        json!([{"tool":"list_windows","arguments":{}},{"tool":"query","arguments":{"selector":{}}},{"tool":"launch_app","arguments":{}}]),
    ];
    for steps in bad_cases {
        let e = b.call_tool("batch", json!({"session":"s1","steps":steps})).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidParams);
        // gm-88s: the third step is "step 3", as the agent and `$3.ref` count it.
        assert!(e.message.contains("batch step 3:"), "{}", e.message);
        assert_eq!(e.data, Some(json!({"step": 3})));
    }
    let many: Vec<Value> = (0..101).map(|_| json!({"tool":"list_windows","arguments":{}})).collect();
    let e = b.call_tool("batch", json!({"session":"s1","steps":many})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = b.call_tool("batch", json!({"session":"s1","steps":[]})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(mock.requests().is_empty(), "nothing reached the agent");
}

#[tokio::test]
async fn batch_capture_step_png_is_extracted_and_retained() {
    let png_bytes = png(4, 4, |_, _| [1, 2, 3, 255]);
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    };
    let result = json!({"steps":[
        {"tool":"list_windows","ok":true,"result":{}},
        {"tool":"capture","ok":true,"result":{"capture_id":"cap-7","target":"w1","width":4,"height":4,"scale":1.0,"stale":false,"annotations":[],"png_base64":b64}},
    ]});
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).respond("batch", result).start(), Policy::default(), 10_000);
    let out = b
        .call_tool("batch", json!({"session":"s1","steps":[{"tool":"list_windows","arguments":{}},{"tool":"capture","arguments":{"target":"w1"}}]}))
        .await
        .unwrap();
    assert_eq!(out.images(), vec![png_bytes.as_slice()]);
    assert!(!out.text_content().contains("png_base64"));
    // retained: a self-diff works
    let d = b.call_tool("diff_captures", json!({"session":"s1","from":"cap-7","to":"cap-7"})).await.unwrap();
    assert_eq!(d.json_content().unwrap()["changed_pixels"], 0);
}
