//! Issue 31: recording must work before unused GTK widget classes initialize.

mod fixture_env;
use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn record_initializes_unused_classes_and_records_minimal_button() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[("FIXTURE_MINIMAL", "1")]);
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(fx.dir.path())).unwrap() };
    c.settle_tree().await;

    let start = c.json("record", json!({})).await;
    assert_eq!(start["recording"], true);
    assert_eq!(start["skipped_hooks"], json!([]), "all supported signals initialize without widget instances: {start}");
    for signal in [
        "GtkButton::clicked",
        "GtkToggleButton::toggled",
        "GtkCheckButton::toggled",
        "GtkSwitch::state-set",
        "GtkEditable::changed",
        "GtkExpander::activate",
        "GtkRange::value-changed",
        "GtkSpinButton::value-changed",
        "GSimpleAction::activate",
        "GSimpleAction::change-state",
    ] {
        assert!(start["hooks"].as_array().unwrap().contains(&json!(signal)), "missing {signal}: {start}");
    }

    c.json("interact", json!({"target": {"automation_id": "minimal-button"}, "operation": "activate"})).await;
    // The direct agent click is ignored. Its deferred echo is an app emission,
    // proving hooks still work without requiring an external input injector.
    let clicked = c.json("wait_for", json!({
        "target": {"automation_id": "minimal-counter"},
        "condition": "text", "equals": "clicks: 2", "timeout_ms": 5000
    })).await;
    assert_eq!(clicked["satisfied"], true, "deferred click did not run: {clicked}");

    let stop = c.json("stop_recording", json!({})).await;
    assert!(stop["ignored_agent_emissions"].as_u64().unwrap() >= 1, "{stop}");
    let emission = stop["app_emissions"].as_array().unwrap().iter().find(|event| {
        event["signal"] == "GtkButton::clicked" && event["target"] == json!({"automation_id": "minimal-button"})
    }).unwrap_or_else(|| panic!("button click was not recorded: {stop}"));
    assert_eq!(emission["step"], json!({
        "tool": "interact", "arguments": {"target": {"automation_id": "minimal-button"}, "operation": "activate"}
    }));
}
