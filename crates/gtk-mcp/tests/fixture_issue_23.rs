//! Issue 23: settled CSS removal must include the theme's return transitions.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use serde_json::{Value, json};

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

async fn shot(c: &Client, target: &Value) -> image::RgbaImage {
    let out = c.call("capture", json!({"target": target})).await.unwrap_or_else(|e| panic!("capture {target}: {e} {:?}", e.data));
    let metadata = out.json_content().expect("capture metadata");
    assert_eq!(metadata["stale"], false, "{metadata}");
    image::load_from_memory_with_format(out.images()[0], image::ImageFormat::Png).expect("valid PNG").to_rgba8()
}

fn marker_pixels(img: &image::RgbaImage) -> usize {
    img.pixels().filter(|p| p[0].abs_diff(0x18) <= 6 && p[1].abs_diff(0xe5) <= 6 && p[2].abs_diff(0xa7) <= 6).count()
}

async fn removal_repaints(c: &Client, name: &str, target: Value, css: &str, exact: bool, iterations: usize) {
    // Prime the cached paintable before applying CSS, not only after applying it.
    let original = shot(c, &target).await;
    assert_eq!(marker_pixels(&original), 0, "{name}: unexpected baseline colour");
    for iteration in 0..iterations {
        let added = c.json("apply_css", json!({"css": css})).await;
        assert_eq!(added["installed"], true, "{name}, iteration {iteration}: {added}");
        assert_eq!(added["settled"], true, "{name}, iteration {iteration}: {added}");
        let styled = shot(c, &target).await;
        assert!(marker_pixels(&styled) > 0, "{name}, iteration {iteration}: provider was not visible");
        let removed = c.json("remove_css", json!({"handle": added["handle"]})).await;
        assert_eq!(removed["removed"], true, "{name}, iteration {iteration}: {removed}");
        assert_eq!(removed["settled"], true, "{name}, iteration {iteration}: {removed}");
        // No sleep, polling, inspect or other round trip between removal and capture.
        let reverted = shot(c, &target).await;
        eprintln!("{name}, iteration {iteration}: remove settled=true, capture stale=false, removed pixels={}", marker_pixels(&reverted));
        assert_eq!(marker_pixels(&reverted), 0, "{name}, iteration {iteration}: removed CSS survived in a capture claiming to be current");
        if exact {
            // Also proves font metrics and border geometry, not just colour, reverted.
            assert!(reverted == original, "{name}, iteration {iteration}: widget did not return to its baseline");
        }
    }
    // An unchanged clean widget must not cost another frame after its CSS
    // generation has been captured, even with a zero wait budget.
    let unchanged = c.json("capture", json!({"target": target, "settle_timeout_ms": 0})).await;
    assert_eq!(unchanged["stale"], false, "{name}: {unchanged}");
    eprintln!("{name}: {iterations} removals settled=true, immediate captures stale=false, removed pixels=0");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_css_repaints_cached_captures() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(fx.dir.path())).unwrap() };
    c.json("interact", json!({"target": by_id("m3-section"), "operation": "scroll_to"})).await;
    let window = c.json("list_windows", json!({})).await["windows"][0]["ref"].clone();
    // Do not disable default theme animations for the regression: removing
    // this provider also removes `transition: none`, exposing return transitions.
    removal_repaints(
        &c, "theme transitions", window.clone(),
        "* { background-color: #18e5a7; background-image: none; transition: none; }",
        false, 24,
    ).await;

    // Static-CSS variations share one fixture and run serially.
    c.json("override_setting", json!({"setting": "animations", "value": false})).await;
    for (name, target, css, exact) in [
        ("class background", by_id("m3-styled"), ".m3-accent { background-color: #18e5a7; background-image: none; }", true),
        ("ID background", by_id("m3-styled"), "#m3-styled { background-color: #18e5a7; }", true),
        ("app-wide provider, widget capture", by_id("m3-styled"), "* { background-color: #18e5a7; background-image: none; }", true),
        ("app-wide provider, window capture", window.clone(), "* { background-color: #18e5a7; background-image: none; }", false),
        ("window background", window, "window { background-color: #18e5a7; background-image: none; }", false),
        ("text colour", by_id("m3-styled"), ".m3-accent { color: #18e5a7; }", true),
        ("font metrics", by_id("m3-styled"), ".m3-accent { color: #18e5a7; font-size: 42px; font-weight: 900; }", true),
        ("border geometry", by_id("m3-styled"), ".m3-accent { border: 12px solid #18e5a7; }", true),
    ] {
        removal_repaints(&c, name, target, css, exact, 8).await;
    }
    c.json("interact", json!({"target": by_id("open-second-window"), "operation": "activate"})).await;
    removal_repaints(&c, "unfocused window", by_id("m3-styled"), ".m3-accent { background-color: #18e5a7; }", true, 8).await;
    c.json("interact", json!({"target": by_id("second-window"), "operation": "close"})).await;

    // A continuously animated native must exhaust the bound honestly, rather
    // than advertise its first animation frame as settled/current.
    c.json("override_setting", json!({"setting": "animations", "value": true})).await;
    let target = by_id("m3-styled");
    shot(&c, &target).await;
    let animated = c.json("apply_css", json!({"css": "@keyframes issue23 { from { opacity: 1; } to { opacity: 0.5; } } .m3-accent { animation: issue23 500ms infinite alternate; }"})).await;
    assert_eq!(animated["installed"], true, "{animated}");
    assert_eq!(animated["settled"], false, "{animated}");
    assert_eq!(animated["settled_reason"], "animating", "{animated}");
    let capture = c.json("capture", json!({"target": target.clone(), "settle_timeout_ms": 100})).await;
    assert_eq!(capture["stale"], true, "{capture}");
    assert_eq!(capture["stale_reason"], "animating", "{capture}");
    let coloured = c.json("apply_css", json!({"css": ".m3-accent { color: #18e5a7; }"})).await;
    assert_eq!(coloured["installed"], true, "{coloured}");
    assert_eq!(coloured["settled"], false, "{coloured}");
    assert_eq!(coloured["settled_reason"], "animating", "{coloured}");
    let still_animated = c.json("remove_css", json!({"handle": coloured["handle"]})).await;
    assert_eq!(still_animated["removed"], true, "{still_animated}");
    assert_eq!(still_animated["settled"], false, "{still_animated}");
    assert_eq!(still_animated["settled_reason"], "animating", "{still_animated}");
    eprintln!("continuous animation: apply/remove settled=false reason=animating; capture stale=true reason=animating");
    let removed = c.json("remove_css", json!({"handle": animated["handle"]})).await;
    assert_eq!(removed["settled"], true, "{removed}");
    let final_frame = shot(&c, &target).await;
    assert_eq!(marker_pixels(&final_frame), 0);
}
