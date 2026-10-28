//! Issue 28: `audit_accessibility` misses widgets that react to clicks
//! without a button role (tray icons: a GtkImage with a GtkGestureClick).
//! The fixture's FIXTURE_TRAY window shows exactly such an icon: clickable,
//! no tooltip/label, accessible role left at the GtkImage default ("img").
//! The audit must flag it (clickable_without_role) and apply the interactive
//! checks (keyboard reach) to it. GTK rules run without AT-SPI, so no bus is
//! needed here.

mod fixture_env;

use fixture_env::*;
use serde_json::{Value, json};

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_flags_clickable_widget_without_interactive_role() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[("FIXTURE_TRAY", "1")]);
    let c = Client { broker: fx.broker() };
    c.settle_tree().await;

    // The icon is in the accessible tree with its non-interactive role, and
    // the agent reports the click gesture on it.
    let tree = c.json("accessible_tree", json!({})).await;
    let icon = find_node(&tree, &|n| n["automation_id"] == "tray-icon").expect("tray icon in the accessible tree");
    assert_eq!(icon["type"], "GtkImage", "{icon}");
    assert_eq!(icon["role"], "img", "{icon}");
    assert_eq!(icon["label"], Value::Null, "the icon deliberately has no label");
    assert_eq!(icon["clickable"], true, "the agent reports the click gesture: {icon}");
    let icon_ref = icon["ref"].as_str().unwrap().to_string();
    for id in ["tray-selectable-label", "tray-link-label"] {
        let label = find_node(&tree, &|n| n["automation_id"] == id).expect("GTK-interactive label in the tray window");
        assert_ne!(label["clickable"], true, "GTK's own label gestures are not app clicks: {label}");
    }
    let tray_window = tree["roots"].as_array().unwrap().iter()
        .find(|root| find_node(root, &|n| n["ref"] == icon_ref).is_some())
        .expect("tray window in the accessible tree");

    let audit = c.json("audit_accessibility", json!({"root": tray_window["ref"]})).await;
    assert!(
        audit["rules"].as_array().unwrap().iter().any(|r| r["name"] == "clickable_without_role" && r["status"] == "ran"),
        "rule ran: {audit}"
    );
    let findings = audit["findings"].as_array().unwrap();
    let f = findings
        .iter()
        .find(|f| f["rule"] == "clickable_without_role" && f["ref"] == icon_ref)
        .unwrap_or_else(|| panic!("clickable_without_role finding for the tray icon: {audit}"));
    assert_eq!(f["type"], "GtkImage");
    assert_eq!(f["role"], "img");
    assert_eq!(f["severity"], "warning");
    assert!(f["message"].as_str().unwrap().contains("'button'"), "{f}");
    assert!(f["message"].as_str().unwrap().contains("label"), "{f}");

    // Clickable widgets get the same checks as interactive ones: the icon is
    // not keyboard reachable and is smaller than the 24x24 px minimum target.
    assert!(findings.iter().any(|f| f["rule"] == "interactive_not_focusable" && f["ref"] == icon_ref), "{audit}");
    assert!(findings.iter().any(|f| f["rule"] == "small_target" && f["ref"] == icon_ref), "{audit}");

    // GTK's own gestures (windows, popovers, labels, scrolling and list views)
    // must not be mistaken for controls added by the application.
    let clickable_findings: Vec<_> = findings.iter().filter(|f| f["rule"] == "clickable_without_role").collect();
    assert_eq!(clickable_findings.len(), 1, "only the tray icon is app-clickable: {audit}");
    assert_eq!(clickable_findings[0]["ref"], icon_ref, "{audit}");

    // The main fixture also deliberately adds a click gesture to a label.
    // Keep reporting that genuine application control, without GTK internals.
    let full = c.json("audit_accessibility", json!({})).await;
    let full_clickable: Vec<_> = full["findings"].as_array().unwrap().iter()
        .filter(|f| f["rule"] == "clickable_without_role").collect();
    let ids: Vec<_> = full_clickable.iter().filter_map(|f| f["evidence"]["automation_id"].as_str()).collect();
    assert!(ids.contains(&"tray-icon") && ids.contains(&"m2-clickable"), "{full}");
    assert_eq!(full_clickable.len(), 2, "only the tray icon and deliberately clickable label are app controls: {full}");

    // A rule filter mentioning the new rule is accepted.
    let scoped = c.json("audit_accessibility", json!({"root": by_id("tray-icon"), "rules": ["clickable_without_role"]})).await;
    let scoped_findings = scoped["findings"].as_array().unwrap();
    assert_eq!(scoped_findings.len(), 1, "{scoped}");
    assert_eq!(scoped_findings[0]["ref"], icon_ref);
}
