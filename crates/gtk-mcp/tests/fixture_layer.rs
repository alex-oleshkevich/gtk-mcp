//! gm-66t regression: layer-shell reporting must only appear on real layer
//! windows. The fixture links libgtk4-layer-shell (via the layer-shell and
//! session-lock crates), so the library is loaded in every test here; a plain
//! window must still report no `layer_shell` section, a session-lock window
//! must report none either (the library stores lock surfaces under the same
//! qdata key as layer surfaces, which used to produce garbage fields), and a
//! real layer panel must report its exact configuration.

mod fixture_env;

use fixture_env::{Client, Fixture};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

async fn windows_until(c: &Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let w = c.json("list_windows", json!({})).await;
        if pred(&w) {
            return w;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; last: {w}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn by_title<'a>(w: &'a Value, title: &str) -> Option<&'a Value> {
    w["windows"]
        .as_array()?
        .iter()
        .find(|win| win["title"] == title)
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_window_has_no_layer_shell_with_library_loaded() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client {
        broker: fx.broker(false),
    };
    // The library really is loaded: the agent advertises the capability.
    let apps = c.json("list_apps", json!({})).await;
    assert!(
        apps[0]["hello"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("layer_shell")),
        "expected layer_shell capability with the library linked: {apps}"
    );
    let w = c.json("list_windows", json!({})).await;
    let win = by_title(&w, "gtk-mcp fixture").expect("main window listed");
    assert!(
        win.get("layer_shell").is_none(),
        "plain window reported layer data: {win}"
    );
    // And it must not match a layer-namespace selector either.
    let q = c
        .json(
            "query",
            json!({"selector": {"window": {"layer_namespace": "fixture-panel"}}}),
        )
        .await;
    assert_eq!(q["total"], 0, "plain window matched a layer namespace: {q}");
}

#[tokio::test(flavor = "multi_thread")]
async fn layer_panel_reports_exact_layer_shell() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[("FIXTURE_LAYER_PANEL", "1")]);
    let c = Client {
        broker: fx.broker(false),
    };
    let w = windows_until(&c, "layer panel", |w| {
        by_title(w, "gtk-mcp fixture panel").is_some_and(|p| p.get("layer_shell").is_some())
    })
    .await;
    let panel = by_title(&w, "gtk-mcp fixture panel").unwrap();
    let l = &panel["layer_shell"];
    assert_eq!(l["namespace"], "fixture-panel", "{l}");
    assert_eq!(l["layer"], "top", "{l}");
    assert_eq!(l["anchors"], json!(["left", "right", "top"]), "{l}");
    assert_eq!(
        l["margins"],
        json!({"left": 10, "right": 20, "top": 30, "bottom": 40}),
        "{l}"
    );
    assert_eq!(l["exclusive_zone"], 42, "{l}");
    assert_eq!(l["keyboard_mode"], "on_demand", "{l}");
    // The plain main window in the same process stays layer-free.
    let main = by_title(&w, "gtk-mcp fixture").expect("main window listed");
    assert!(
        main.get("layer_shell").is_none(),
        "plain window reported layer data: {main}"
    );
    // The namespace selector finds the panel.
    let q = c
        .json(
            "query",
            json!({"selector": {"window": {"layer_namespace": "fixture-panel"}}}),
        )
        .await;
    assert!(
        q["total"].as_u64().unwrap() >= 1,
        "panel not found by namespace: {q}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lock_window_has_no_layer_shell() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[("FIXTURE_SESSION_LOCK", "1")]);
    let c = Client {
        broker: fx.broker(false),
    };
    let w = windows_until(&c, "session-lock window", |w| {
        by_title(w, "gtk-mcp fixture lock").is_some()
    })
    .await;
    let lock = by_title(&w, "gtk-mcp fixture lock").unwrap();
    assert!(
        lock.get("layer_shell").is_none(),
        "session-lock window reported layer data (gm-66t): {lock}"
    );
    let main = by_title(&w, "gtk-mcp fixture").expect("main window listed");
    assert!(
        main.get("layer_shell").is_none(),
        "plain window reported layer data: {main}"
    );
}
