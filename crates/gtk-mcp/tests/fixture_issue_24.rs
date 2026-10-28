//! Issue 24: snapshot's target alias scopes the tree and typos are rejected.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::{ErrorCode, SnapshotParams, rpc::Request};
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_target_window_alias_matches_root_and_excludes_other_windows() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(fx.dir.path())).unwrap() };
    c.json("interact", json!({"target": {"automation_id": "open-second-window"}, "operation": "activate"})).await;
    c.settle_tree().await;

    let all = c.json("snapshot", json!({"format": "json", "max_depth": 1})).await;
    assert!(all["roots"].as_array().unwrap().len() > 1, "need multiple toplevels: {all}");
    let window = json!({"window": {"title": "Second window"}});
    let root = c.json("snapshot", json!({"root": window, "format": "json", "max_depth": 1})).await;
    let target = c.json("snapshot", json!({"target": window, "format": "json", "max_depth": 1})).await;
    assert_eq!(root["roots"].as_array().unwrap().len(), 1, "{root}");
    assert_eq!(target["roots"], root["roots"], "target must select the same window as root");
    assert_eq!(target["node_count"], root["node_count"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_unknown_parameters_fail_before_session_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(dir.path())).unwrap() };
    let e = c.err("snapshot", json!({"bogus": true, "format": "json"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(e.message.contains("unknown field") && e.message.contains("bogus"), "{e}");
}

#[test]
fn snapshot_alias_preserves_session_and_transport_metadata() {
    let request = Request::new(
        1,
        "snapshot",
        json!({"session": "s1", "target": {"window": {"title": "Second window"}}, "format": "json"}),
        Some(1500),
    );
    let params: SnapshotParams = request.parse_params().unwrap();
    assert_eq!(params.session, "s1");
    let serialized = serde_json::to_value(params).unwrap();
    assert_eq!(serialized["root"], json!({"window": {"title": "Second window"}}));
    assert!(serialized.get("target").is_none());
    assert!(serde_json::from_value::<SnapshotParams>(json!({"session": "s1", "bogus": true})).is_err());
    assert!(serde_json::from_value::<SnapshotParams>(json!({"root": "w1", "target": "w2"})).is_err());
    assert_eq!(request.params["_meta"]["deadline_ms"], Value::from(1500));
}
