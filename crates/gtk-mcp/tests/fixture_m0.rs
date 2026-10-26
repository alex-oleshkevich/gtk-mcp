//! M0 acceptance against the real fixture: AC-1 (real app), AC-2, AC-3
//! (stale/detached parts that need no interaction), AC-6 (snapshot/inspect part).

mod fixture_env;

use fixture_env::*;
use gtk_mcp_protocol::{ErrorCode, SessionState};
use serde_json::{Value, json};

const SECRET: &str = "hunter2-SECRET-0xC0FFEE";

#[tokio::test(flavor = "multi_thread")]
async fn discovery_real_fixture_is_listed_running() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    let apps = c.json("list_apps", json!({})).await;
    let apps = apps.as_array().unwrap();
    assert_eq!(apps.len(), 1, "{apps:?}");
    assert_eq!(apps[0]["pid"], fx.pid);
    assert_eq!(apps[0]["app_id"], "dev.gtkmcp.Fixture");
    let info: gtk_mcp_protocol::SessionInfo = serde_json::from_value(apps[0].clone()).unwrap();
    assert_eq!(info.state, SessionState::Running);
    let hello = info.hello.unwrap();
    assert_eq!(hello.gtk_version[0], 4);
    assert_eq!(hello.adw_version.map(|v| v[0]), Some(1));
    assert!(!hello.mutations_enabled);
    assert!(hello.capabilities.contains(&gtk_mcp_protocol::Capability::Adwaita));
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_killed_fixture_leaves_no_listing_and_socket_is_removed() {
    let display = require_display!();
    let mut fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    assert_eq!(c.json("list_apps", json!({})).await.as_array().unwrap().len(), 1);
    let sock = fx.socket();
    // SIGKILL: the socket file is left behind by the dead process.
    fx.child.kill().unwrap();
    fx.child.wait().unwrap();
    // A fresh broker (new process view) must not list it and must unlink the file.
    let c2 = Client { broker: fx.broker(false) };
    let apps = c2.json("list_apps", json!({})).await;
    assert!(apps.as_array().unwrap().is_empty(), "{apps}");
    assert!(!sock.exists(), "stale socket must be removed");
    // The first broker's session reports Exited.
    c.broker.rescan().await;
    let apps = c.json("list_apps", json!({})).await;
    assert!(
        apps.as_array().unwrap().iter().all(|a| a["state"].get("exited").is_some()),
        "{apps}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_inactive_without_gtk_mcp_env_and_env_is_scrubbed() {
    let display = require_display!();
    // GTK_MCP=0 -> no socket at all. Run it directly (Fixture::start waits for a socket).
    let dir = tempfile::tempdir().unwrap();
    let bin = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("gtk-mcp-fixture");
    let _ = Fixture::start(&display, false, &[]); // ensures the binary is built
    let mut child = std::process::Command::new(&bin)
        .env("GTK_MCP", "0")
        .env("GTK_MCP_DIR", dir.path())
        .env("XDG_RUNTIME_DIR", &display.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &display.wayland_display)
        .env("GDK_BACKEND", "wayland")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "inactive agent must not bind");
    let _ = child.kill();
    let _ = child.wait();

    // With GTK_MCP=1 the variables are removed from the live process environment
    // (the fixture reports what `std::env` sees in a label).
    let fx = Fixture::start(&display, true, &[]);
    let initial = std::fs::read(format!("/proc/{}/environ", fx.pid)).unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("GTK_MCP=1"), "set at launch");
    let c = Client { broker: fx.broker(false) };
    let s = c.snapshot_json(json!({})).await;
    assert_eq!(node_by_name(&s, "env-report")["text"], "gtk-mcp-env: none");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_refs_are_stable_and_text_matches_json() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    c.settle_tree().await;
    let a = c.snapshot_json(json!({})).await;
    let b = c.snapshot_json(json!({})).await;
    assert_ne!(a["snapshot_id"], b["snapshot_id"]);
    let (mut ra, mut rb) = (vec![], vec![]);
    count_refs(&a, &mut ra);
    count_refs(&b, &mut rb);
    assert_eq!(ra, rb, "same live widgets carry the same refs and hierarchy");
    assert!(ra.len() > 50);

    // Text and JSON describe the same hierarchy and refs.
    let text = c.call("snapshot", json!({"format": "text"})).await.unwrap().text_content().to_string();
    for r in &ra {
        assert_eq!(text.matches(&format!("[{r}]")).count(), 1, "ref {r} must appear exactly once in text");
    }
    assert!(text.contains("snapshot_id: snap-"));
    // indentation depth in text equals JSON depth
    fn depths(v: &Value, d: usize, out: &mut Vec<(String, usize)>) {
        if let Some(r) = v["ref"].as_str() {
            out.push((r.to_string(), d));
        }
        for ch in v["children"].as_array().into_iter().flatten() {
            depths(ch, d + 1, out);
        }
    }
    let mut dj = vec![];
    for r in a["roots"].as_array().unwrap() {
        depths(r, 0, &mut dj);
    }
    for (r, d) in dj {
        let line = text.lines().find(|l| l.contains(&format!("[{r}]"))).unwrap();
        let indent = line.chars().take_while(|c| " │├└─".contains(*c)).count();
        assert_eq!(indent, d * 4, "depth of {r} in: {line}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_collapsed_expander_child_is_detached_via_adapter() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    let s = c.snapshot_json(json!({})).await;
    let exp = node_by_name(&s, "advanced-expander");
    assert_eq!(exp["type"], "GtkExpander");
    assert_eq!(exp["state"]["expanded"], false);
    let child = exp["children"].as_array().unwrap().iter().find(|c| c["attached"] == false).expect("detached child listed");
    assert_eq!(exp["children"].as_array().unwrap().last().unwrap()["ref"], child["ref"], "detached content comes after real children");
    assert_eq!(child["attached"], false);
    assert_eq!(child["via"], "GtkExpander:child");
    assert_eq!(child["identity"]["automation_id"], "server-address");
    assert_eq!(child["state"]["mapped"], false);
    assert_eq!(child["text"], "localhost");
    let text = c.call("snapshot", json!({})).await.unwrap().text_content().to_string();
    assert!(text.contains("detached via=GtkExpander:child"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_popover_in_place_list_realized_truncation() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    let s = c.snapshot_json(json!({})).await;

    // closed popover appears once, in place, native + collapsed with a child count
    let mb = node_by_name(&s, "menu-button");
    let in_button: Vec<_> = mb["children"].as_array().unwrap().iter().filter(|n| n["native"] == true).collect();
    assert_eq!(in_button.len(), 1);
    assert_eq!(in_button[0]["children_collapsed"], true);
    assert!(in_button[0]["child_count"].as_u64().unwrap() >= 1);
    assert!(in_button[0]["children"].as_array().unwrap().is_empty());
    let popover_ref = in_button[0]["ref"].as_str().unwrap();
    let mut refs = vec![];
    count_refs(&s, &mut refs);
    assert_eq!(refs.iter().filter(|r| *r == popover_ref).count(), 1, "popover must not be repeated as a root");
    // expanding closed popovers on request
    let s2 = c.snapshot_json(json!({"include_closed_popovers": true})).await;
    let mb2 = node_by_name(&s2, "menu-button");
    let pop2 = mb2["children"].as_array().unwrap().iter().find(|n| n["native"] == true).unwrap();
    assert!(!pop2["children"].as_array().unwrap().is_empty());
    assert!(pop2.get("children_collapsed").is_none());

    // list view with 500 items reports realized children
    let list = node_by_name(&s, "items-list");
    assert_eq!(list["type"], "GtkListView");
    assert_eq!(list["children_source"], "realized");
    assert!(list["children"].as_array().unwrap().len() < 500);
    assert!(!list["children"].as_array().unwrap().is_empty());

    // truncation is explicit
    let t = c.snapshot_json(json!({"max_nodes": 20})).await;
    assert_eq!(t["truncated"], true);
    assert_eq!(t["complete"], false);
    assert!(t["node_count"].as_u64().unwrap() <= 21, "{}", t["node_count"]);
    let cut = find_node(&t, &|n| n["children_truncated"] == true).expect("a cut node is marked");
    assert!(cut["omitted_children"].as_u64().unwrap() >= 1);
    let td = c.snapshot_json(json!({"max_depth": 1})).await;
    assert_eq!(td["truncated"], true);
    // text_limit
    let tl = c.snapshot_json(json!({"text_limit": 4})).await;
    let lbl = node_by_name(&tl, "long-label");
    assert_eq!(lbl["text"].as_str().unwrap().chars().count(), 4);
    assert_eq!(lbl["text_truncated"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_semantic_view_keeps_hierarchy_and_marks_elision() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    c.settle_tree().await;
    let full = c.snapshot_json(json!({})).await;
    let sem = c.snapshot_json(json!({"view": "semantic"})).await;
    assert!(sem["node_count"].as_u64().unwrap() < full["node_count"].as_u64().unwrap());
    let save = node_by_name(&sem, "save-primary");
    assert_eq!(save["label"], "Save");
    let (mut full_refs, mut sem_refs) = (vec![], vec![]);
    count_refs(&full, &mut full_refs);
    count_refs(&sem, &mut sem_refs);
    assert!(sem_refs.iter().all(|r| full_refs.contains(r)), "semantic view never invents nodes");
    assert!(find_node(&sem, &|n| n["elided_children"].as_u64().unwrap_or(0) > 0).is_some());
    let text = c.call("snapshot", json!({"view": "semantic"})).await.unwrap().text_content().to_string();
    assert!(text.contains("structural widgets"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_snapshot_query_inspect_never_contain_content() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = Client { broker: fx.broker(true) };
    // The password is placed through the agent's own mutating path in M1 tests
    // (secrets_*); here only the structure/redaction flags are asserted.
    let s = c.snapshot_json(json!({})).await;
    let pw = node_by_name(&s, "password");
    assert_eq!(pw["secret"], true);
    assert_eq!(pw["text_redacted"], true);
    assert!(pw.get("text").is_none());
    let row = node_by_name(&s, "password-row");
    assert_eq!(row["secret"], true);
    assert_eq!(row["text_redacted"], true);
    // descendants of a secret root (GtkText delegate) are secret too
    let inner = find_node(pw, &|n| n["type"] == "GtkText").expect("password entry has a GtkText delegate");
    assert_eq!(inner["secret"], true);
    assert!(inner.get("text").is_none());
    let text = c.call("snapshot", json!({})).await.unwrap().text_content().to_string();
    assert!(text.contains("secret text=<redacted>"), "{text}");
    assert!(!text.contains(SECRET));
    // a secret never matches text selectors
    let q = c.json("query", json!({"selector": {"text": ""}})).await;
    assert!(q["matches"].as_array().unwrap().iter().all(|m| m["ref"] != pw["ref"]));
    let i = c.json("inspect", json!({"target": pw["ref"]})).await;
    assert_eq!(i["secret"], true);
    assert_eq!(i["text_redacted"], true);
    assert!(i.get("text").is_none());
    assert!(i["related"].as_object().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn targets_query_inspect_and_ambiguity() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    // AC-3: two "Save" buttons -> ambiguous via text selector in a single-target tool
    let e = c.err("inspect", json!({"target": {"text": "Save", "type_name": "GtkButton"}})).await;
    assert_eq!(e.code, ErrorCode::AmbiguousTarget);
    let cands = e.data.as_ref().unwrap()["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 2);
    let names: Vec<_> = cands.iter().map(|c| c["automation_id"].as_str().unwrap()).collect();
    assert!(names.contains(&"save-primary") && names.contains(&"save-secondary"));
    // nth disambiguates in tree order
    let i0 = c.json("inspect", json!({"target": {"text": "Save", "type_name": "GtkButton", "nth": 0}})).await;
    let i1 = c.json("inspect", json!({"target": {"text": "Save", "type_name": "GtkButton", "nth": 1}})).await;
    assert_eq!(i0["identity"]["automation_id"], "save-primary");
    assert_eq!(i1["identity"]["automation_id"], "save-secondary");
    // query lists both, with total
    let q = c.json("query", json!({"selector": {"text": "Save", "type_name": "GtkButton"}})).await;
    assert_eq!(q["total"], 2);
    // AND semantics + css/state/ancestor keys
    let q = c.json("query", json!({"selector": {"automation_id": "save-primary", "text": "Nope"}})).await;
    assert_eq!(q["total"], 0);
    let q = c.json("query", json!({"selector": {"type_name": "GtkWidget", "automation_id": "feature-switch"}})).await;
    assert_eq!(q["total"], 1, "type_name matches ancestors");
    let q = c.json("query", json!({"selector": {"type_name": "GtkButton", "ancestor": {"automation_id": "victim-box"}}})).await;
    assert_eq!(q["total"], 0);
    let q = c.json("query", json!({"selector": {"automation_id": "submit", "state": {"sensitive": false}}})).await;
    assert_eq!(q["total"], 1);
    // window by title and by app_window_id; monitor of an unmapped query is absent
    let q = c.json("query", json!({"selector": {"window": {"title": "gtk-mcp fixture"}, "automation_id": "volume"}})).await;
    assert_eq!(q["total"], 1);
    let q = c.json("query", json!({"selector": {"window": {"app_window_id": 1}, "automation_id": "volume"}})).await;
    assert_eq!(q["total"], 1);
    let q = c.json("query", json!({"selector": {"window": {"title": "nope"}, "automation_id": "volume"}})).await;
    assert_eq!(q["total"], 0);
    let wins = c.json("list_windows", json!({})).await;
    let connector = wins["windows"][0]["monitor"].as_str().expect("monitor connector").to_string();
    let q = c.json("query", json!({"selector": {"window": {"monitor": connector}, "automation_id": "volume"}})).await;
    assert_eq!(q["total"], 1);
    // errors
    let e = c.err("inspect", json!({"target": "w99999"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("inspect", json!({"target": {"automation_id": "does-not-exist"}})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("inspect", json!({"target": "banana"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
}

#[tokio::test(flavor = "multi_thread")]
async fn windows_list_window_and_layer_data_absent_without_layer_shell() {
    let display = require_display!();
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(false) };
    let w = c.json("list_windows", json!({})).await;
    let wins = w["windows"].as_array().unwrap();
    assert_eq!(wins.len(), 1);
    let win = &wins[0];
    assert_eq!(win["title"], "gtk-mcp fixture");
    assert_eq!(win["visible"], true);
    assert!(win["renderer"].as_str().unwrap().contains("Renderer"));
    assert!(win["surface_scale"].as_i64().unwrap() >= 1);
    assert!(win.get("layer_shell").is_none(), "no layer data when gtk4-layer-shell is not loaded");
    assert!(win["ref"].as_str().unwrap().starts_with('w'));
    // the window ref resolves in snapshot roots
    let s = c.snapshot_json(json!({"root": win["ref"]})).await;
    assert_eq!(s["roots"][0]["ref"], win["ref"]);
    assert_eq!(s["roots"][0]["root_kind"], "window");
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_remote_application_instance_never_binds() {
    let display = require_display!();
    // Unique application id: the first instance owns the name and binds.
    let first = Fixture::start(&display, false, &[("FIXTURE_UNIQUE", "1")]);
    // Second instance: same app id, shared session bus name -> remote instance, no `startup`.
    let dir2 = tempfile::tempdir().unwrap();
    let bin = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("gtk-mcp-fixture");
    let mut second = std::process::Command::new(&bin)
        .env("GTK_MCP", "1")
        .env("GTK_MCP_DIR", dir2.path())
        .env("FIXTURE_UNIQUE", "1")
        .env("XDG_RUNTIME_DIR", &display.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &display.wayland_display)
        .env("GDK_BACKEND", "wayland")
        .env("GTK_A11Y", "none")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let remote_bound = std::fs::read_dir(dir2.path()).unwrap().next().is_some();
    let exited = second.try_wait().unwrap();
    assert!(exited.is_some(), "a remote instance forwards activate to the primary and exits");
    let _ = second.kill();
    let _ = second.wait();
    // The remote instance only forwards `activate` and exits; it must not have bound.
    assert!(!remote_bound, "a remote GApplication instance must never bind a socket");
    assert!(first.socket().exists());
}
