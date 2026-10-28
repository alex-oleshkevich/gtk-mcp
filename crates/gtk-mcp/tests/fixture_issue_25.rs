//! Issue 25: one item budget covers shortcut controllers and app accelerators.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use serde_json::{Value, json};

fn item_count(result: &Value) -> usize {
    result["controllers"].as_array().unwrap().len() + result["app_accels"].as_array().map_or(0, Vec::len)
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_shortcuts_limits_all_entries_and_reports_the_uncut_total() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(fx.dir.path())).unwrap() };
    c.settle_tree().await;

    let full = c.json("inspect_shortcuts", json!({"max_items": u32::MAX})).await;
    let total = item_count(&full);
    let controllers = full["controllers"].as_array().unwrap().len();
    assert!(total > 5, "fixture must exercise truncation: {full}");
    assert!(!full["app_accels"].as_array().unwrap().is_empty(), "fixture has application accelerators");
    assert_eq!(full["total"].as_u64().unwrap() as usize, total);
    assert_eq!(full["truncated"], false);

    let limited = c.json("inspect_shortcuts", json!({"max_items": 5})).await;
    assert_eq!(item_count(&limited), 5, "{limited}");
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["total"], full["total"]);

    // Exhausting the budget on controllers must also omit app accelerators.
    let boundary = c.json("inspect_shortcuts", json!({"max_items": controllers})).await;
    assert_eq!(item_count(&boundary), controllers);
    assert!(boundary["app_accels"].as_array().unwrap().is_empty(), "{boundary}");
    assert_eq!(boundary["truncated"], true);
    assert_eq!(boundary["total"], full["total"]);

    let zero = c.json("inspect_shortcuts", json!({"max_items": 0})).await;
    assert_eq!(item_count(&zero), 0);
    assert_eq!(zero["truncated"], true);
    assert_eq!(zero["total"], full["total"]);

    let exact = c.json("inspect_shortcuts", json!({"max_items": total})).await;
    assert_eq!(item_count(&exact), total);
    assert_eq!(exact["truncated"], false);

    let default = c.json("inspect_shortcuts", json!({})).await;
    assert_eq!(item_count(&default), total.min(200), "{default}");
    assert_eq!(default["truncated"], total > 200);
    assert_eq!(default["total"], full["total"]);

    // The same budget applies to a subtree, without adding app accelerators.
    let scoped = c.json("inspect_shortcuts", json!({"target": {"automation_id": "m2-shortcut-entry"}, "max_items": 0})).await;
    assert_eq!(item_count(&scoped), 0);
    assert!(scoped.get("app_accels").is_none());
    assert!(scoped["total"].as_u64().unwrap() > 0);
    assert_eq!(scoped["truncated"], true);
}
