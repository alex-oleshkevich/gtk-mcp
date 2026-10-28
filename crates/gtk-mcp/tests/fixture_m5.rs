//! M5 acceptance against the real fixture through the real broker: `watch`,
//! `read_events`, `unwatch`, `read_logs`, `measure_frames`, `record`,
//! `stop_recording` (AC-6 watch part, AC-14). Input for the AC-14 checks is
//! real: wayland-mcp's virtual keyboard/pointer against the test compositor.
//! Every wait is bounded; the harness watchdog turns any hang into a failure.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::ErrorCode;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SECRET: &str = "hunter2-SECRET-xyz";

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

fn policy() -> Policy {
    Policy { allow_launch: true, ..Policy::default() }
}

fn client(fx: &Fixture) -> (Client, Arc<Broker>) {
    let b = Broker::new(policy(), Config::with_socket_dir(fx.dir.path())).unwrap();
    (Client { broker: b.clone() }, b)
}

async fn until<T>(what: &str, within: Duration, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let end = Instant::now() + within;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < end, "timed out after {within:?} waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn events(c: &Client, args: Value) -> Value {
    c.json("read_events", args).await
}

/// Events (from cursor 0) matching `pred`, waiting up to 10 s for at least one.
async fn wait_event(c: &Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    until(what, Duration::from_secs(10), async || {
        let r = events(c, json!({})).await;
        r["entries"].as_array().unwrap().iter().find(|e| pred(e)).cloned()
    })
    .await
}

async fn text_of(c: &Client, id: &str) -> String {
    c.json("inspect", json!({"target": by_id(id)})).await["text"].as_str().unwrap_or_default().to_owned()
}

// ---------------------------------------------------------------- watch

#[tokio::test(flavor = "multi_thread")]
async fn watch_toggling_the_switch_produces_a_notify_event() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    let w = c.json("watch", json!({"target": by_id("feature-switch"), "kind": "notify", "properties": ["active"]})).await;
    let sub = w["subscription"].as_str().unwrap().to_owned();
    assert_eq!(w["kind"], "notify");
    c.json("interact", json!({"target": by_id("feature-switch"), "operation": "set_active", "active": true})).await;
    let ev = wait_event(&c, "notify active", |e| e["kind"] == "notify" && e["property"] == "active").await;
    assert_eq!(ev["subscription"], sub);
    assert_eq!(ev["value"], true);
    assert_eq!(ev["type"], "GtkSwitch");
    assert_eq!(ev["by_agent"], true, "caused by the agent's own interact");
    assert!(ev["time_ms"].as_u64().unwrap() > 0);

    // unwatch stops further events
    let r = c.json("unwatch", json!({"subscription": sub})).await;
    assert_eq!(r["removed"], true);
    let before = events(&c, json!({})).await["next_cursor"].as_u64().unwrap();
    c.json("interact", json!({"target": by_id("feature-switch"), "operation": "set_active", "active": false})).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(events(&c, json!({"since": before})).await["entries"].as_array().unwrap().len(), 0);
    assert_eq!(c.err("unwatch", json!({"subscription": sub})).await.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_notify_on_a_secret_entry_carries_names_only() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    let w = c.json("watch", json!({"target": by_id("password"), "kind": "notify", "properties": ["text"]})).await;
    assert_eq!(w["secret"], true);
    // all properties of the secret entry as well
    c.json("watch", json!({"target": by_id("password"), "kind": "notify"})).await;
    c.json("interact", json!({"target": by_id("password"), "operation": "fill", "text": SECRET})).await;
    let ev = wait_event(&c, "notify text", |e| e["kind"] == "notify" && e["property"] == "text").await;
    assert_eq!(ev["secret"], true);
    assert!(ev.get("value").is_none(), "secret events carry no value: {ev}");
    let raw = serde_json::to_string(&events(&c, json!({})).await).unwrap();
    assert!(!raw.contains(SECRET), "the secret string must be absent from every response byte");
    let all = events(&c, json!({})).await;
    for e in all["entries"].as_array().unwrap() {
        assert!(e.get("value").is_none(), "no secret event may carry a value: {e}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_signal_hooks_clicked_and_refuses_notify_and_unknown_signals() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    let w = c.json("watch", json!({"target": by_id("save-primary"), "kind": "signal", "signal": "clicked"})).await;
    assert_eq!(w["signal"], "clicked");
    // an emission on another button is not reported
    c.json("interact", json!({"target": by_id("save-secondary"), "operation": "activate"})).await;
    c.json("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    let ev = wait_event(&c, "clicked signal", |e| e["kind"] == "signal").await;
    assert_eq!(ev["signal"], "clicked");
    assert_eq!(ev["target"], w["target"]);
    assert_eq!(ev["args"], json!([]));
    let all = events(&c, json!({"kind": "signal"})).await;
    assert_eq!(all["entries"].as_array().unwrap().len(), 1, "{all}");

    assert_eq!(c.err("watch", json!({"target": by_id("save-primary"), "kind": "signal", "signal": "notify"})).await.code, ErrorCode::InvalidParams);
    assert_eq!(c.err("watch", json!({"target": by_id("save-primary"), "kind": "signal", "signal": "no-such-signal"})).await.code, ErrorCode::InvalidParams);
    assert_eq!(c.err("watch", json!({"target": by_id("save-primary"), "kind": "notify", "properties": ["nope"]})).await.code, ErrorCode::InvalidParams);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_model_reports_items_and_selection_changes() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    c.settle_tree().await;
    let w = c.json("watch", json!({"target": by_id("watch-list"), "kind": "model"})).await;
    assert_eq!(w["selection_changed"], true);
    c.json("interact", json!({"target": by_id("watch-append"), "operation": "activate"})).await;
    let ev = wait_event(&c, "items_changed", |e| e["kind"] == "items_changed").await;
    assert_eq!((ev["position"].clone(), ev["removed"].clone(), ev["added"].clone(), ev["n_items"].clone()), (json!(2), json!(0), json!(1), json!(3)));
    // selection through the list's model
    let m = c.json("inspect_model", json!({"target": by_id("watch-list")})).await;
    let sel = m["selection"].clone();
    assert!(!sel.is_null(), "{m}");
    c.json("interact", json!({"target": by_id("watch-list"), "operation": "select", "index": 1})).await;
    wait_event(&c, "selection_changed", |e| e["kind"] == "selection_changed").await;
}

// ---------------------------------------------------------------- logs

fn fixture_argv() -> Vec<String> {
    vec![fixture_binary().to_string_lossy().into_owned()]
}

fn launch_json(d: &Display) -> Value {
    json!({
        "argv": fixture_argv(),
        "env": {"GSETTINGS_BACKEND": "memory", "GDK_BACKEND": "wayland"},
        "presets": {"a11y": "none"},
        "wayland_display": d.wayland_display,
        "xdg_runtime_dir": d.xdg_runtime_dir,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_a_gtk_critical_in_the_launched_fixture_appears_in_read_logs() {
    let display = require_display!();
    let dir = tempfile::tempdir().unwrap();
    let b = Broker::new(policy(), Config::with_socket_dir(dir.path())).unwrap();
    let c = Client { broker: b.clone() };
    let r = c.json("launch_app", launch_json(&display)).await;
    assert_eq!(r["state"], "running", "{r}");
    c.json("interact", json!({"target": by_id("log-critical"), "operation": "activate"})).await;
    c.json("interact", json!({"target": by_id("log-warning"), "operation": "activate"})).await;
    let rec = until("critical record in read_logs", Duration::from_secs(10), async || {
        let r = c.json("read_logs", json!({"min_level": "critical"})).await;
        r["entries"].as_array().unwrap().iter().find(|e| e["message"].as_str().is_some_and(|m| m.contains("m5-critical-marker 42"))).cloned()
    })
    .await;
    assert_eq!(rec["level"], "critical");
    assert_eq!(rec["domain"], "FixtureM5");
    assert_eq!(rec["source"], "stderr");
    // levels below the filter are excluded, but present without it
    let warn_only = c.json("read_logs", json!({"min_level": "critical", "contains": "m5-warning-marker"})).await;
    assert!(warn_only["entries"].as_array().unwrap().is_empty());
    let any = c.json("read_logs", json!({"contains": "m5-warning-marker", "source": "stderr"})).await;
    assert_eq!(any["entries"][0]["level"], "warning");
    // the cursor resumes after the last record
    let next = any["next_cursor"].as_u64().unwrap();
    assert!(c.json("read_logs", json!({"since": next})).await["entries"].as_array().unwrap().iter().all(|e| e["cursor"].as_u64().unwrap() >= next));
    b.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_external_app_without_forwarding_is_unavailable_and_with_forwarding_works() {
    let display = require_display!();
    let plain = Fixture::start(&display, &[]);
    let (c, _b) = client(&plain);
    let e = c.err("read_logs", json!({})).await;
    assert_eq!(e.code, ErrorCode::Unavailable, "{e}");
    drop(plain);

    let fwd = Fixture::start(&display, &[("FIXTURE_LOG_WRITER", "1")]);
    let (c, _b) = client(&fwd);
    c.json("interact", json!({"target": by_id("log-critical"), "operation": "activate"})).await;
    let rec = until("forwarded critical record", Duration::from_secs(10), async || {
        let r = c.call("read_logs", json!({"source": "forwarded"})).await.ok()?.json_content().ok()?;
        r["entries"].as_array()?.iter().find(|e| e["message"].as_str().is_some_and(|m| m.contains("m5-critical-marker"))).cloned()
    })
    .await;
    assert_eq!(rec["level"], "critical");
    assert_eq!(rec["domain"], "FixtureM5");
    assert_eq!(rec["source"], "forwarded");
    assert!(rec["fields"].is_object());
    let apps = c.json("list_apps", json!({})).await;
    assert!(apps[0]["hello"]["capabilities"].as_array().is_some());
}

// ---------------------------------------------------------------- frames

#[tokio::test(flavor = "multi_thread")]
async fn measure_frames_of_an_animating_widget_has_timings() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    c.json("interact", json!({"target": by_id("animate-toggle"), "operation": "set_active", "active": true})).await;
    let r = c.json("measure_frames", json!({"duration_ms": 1500})).await;
    assert_eq!(r["idle"], false, "{r}");
    let frames = r["frames"].as_u64().unwrap();
    assert!(frames >= 2, "an animating widget paints repeatedly: {r}");
    assert!(r["window_ms"].as_u64().unwrap() >= 1400);
    assert!(r["frame_intervals"]["avg_ms"].as_f64().unwrap() > 0.0, "{r}");
    assert!(r["refresh_interval_ms"].as_f64().unwrap() > 0.0);
    assert_eq!(r["timings"].as_array().unwrap().len() as u64, frames.min(120));
    assert!(r["timings"][0]["frame_time_us"].as_i64().unwrap() > 0);
    assert!(r["presentation"]["available"].is_boolean());

    // the animated widget as the target works too
    let r2 = c.json("measure_frames", json!({"target": by_id("animated-area"), "duration_ms": 600})).await;
    assert!(r2["frames"].as_u64().unwrap() >= 1, "{r2}");

    assert_eq!(c.err("measure_frames", json!({"duration_ms": 0})).await.code, ErrorCode::InvalidParams);
    assert_eq!(c.err("measure_frames", json!({"duration_ms": 60_000})).await.code, ErrorCode::InvalidParams);
}

// ---------------------------------------------------------------- record

#[tokio::test(flavor = "multi_thread")]
async fn record_ignores_agent_actions_and_lists_app_emissions_with_secret_placeholders() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    let r = c.json("record", json!({})).await;
    assert_eq!(r["recording"], true);
    assert!(r["controllers"].as_u64().unwrap() >= 1);
    assert_eq!(c.err("record", json!({})).await.code, ErrorCode::InvalidParams, "one recording at a time");

    // the agent's own actions are ignored
    c.json("interact", json!({"target": by_id("name-entry"), "operation": "fill", "text": "agent typed"})).await;
    c.json("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    // an emission the app causes by itself (after the 300 ms input window)
    c.json("interact", json!({"target": by_id("emit-later"), "operation": "activate"})).await;
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let s = c.json("stop_recording", json!({})).await;
    assert_eq!(s["steps"], 0, "no human input happened: {s}");
    assert!(s["ignored_agent_emissions"].as_u64().unwrap() >= 2, "{s}");
    let app = s["app_emissions"].as_array().unwrap();
    // set_text emits `changed` for the deletion and for the insertion
    assert!(
        app.iter().any(|e| e["signal"] == "GtkEditable::changed" && e["target"] == by_id("name-entry") && e["step"]["arguments"]["text"] == "typed-by-the-app"),
        "{s}"
    );
    assert!(app.iter().any(|e| e["signal"] == "GtkButton::clicked" && e["target"] == by_id("submit")), "{s}");
    let secret = app.iter().find(|e| e["target"] == by_id("password") && e["step"]["arguments"]["text"] == "{{secret:1}}");
    assert!(secret.is_some(), "secret emission listed with a placeholder: {s}");
    assert_eq!(s["secrets"][0]["placeholder"], "{{secret:1}}");
    assert_eq!(s["secrets"][0]["target"], by_id("password"));
    assert!(!serde_json::to_string(&s).unwrap().contains("app-secret-9f31"), "secret text must not appear anywhere");

    assert_eq!(c.err("stop_recording", json!({})).await.code, ErrorCode::NotFound);
}

/// wayland-mcp driving the test compositor's keyboard and pointer.
struct RealInput {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

fn wayland_mcp_binary() -> Option<PathBuf> {
    let p = std::env::var_os("GTK_MCP_TEST_WAYLAND_MCP")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/alex/projects/wayland-mcp/target/release/wayland-mcp"));
    p.exists().then_some(p)
}

impl RealInput {
    fn start(bin: &Path, d: &Display) -> RealInput {
        let mut child = Command::new(bin)
            .env("XDG_RUNTIME_DIR", &d.xdg_runtime_dir)
            .env("WAYLAND_DISPLAY", &d.wayland_display)
            .env_remove("NIRI_SOCKET")
            .env_remove("DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wayland-mcp");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut r = RealInput { child, stdin, out, next: 1 };
        r.rpc("initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "gtk-mcp-test", "version": "0"}}));
        writeln!(r.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        r.stdin.flush().unwrap();
        r
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "wayland-mcp closed its output");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let v = self.rpc("tools/call", json!({"name": tool, "arguments": args}));
        assert!(v["error"].is_null() && v["result"]["isError"] != true, "wayland-mcp {tool} {args} failed: {v}");
        v
    }
}

impl Drop for RealInput {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn focus(c: &Client, id: &str) {
    c.json("interact", json!({"target": by_id(id), "operation": "focus"})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn record_real_input_becomes_a_batch_that_replays_in_a_new_session() {
    let display = require_display!();
    let Some(bin) = wayland_mcp_binary() else {
        eprintln!("record_real_input_*: SKIPPED, wayland-mcp binary not found (set GTK_MCP_TEST_WAYLAND_MCP)");
        return;
    };
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    c.settle_tree().await;
    let start = c.json("record", json!({})).await;
    assert_eq!(start["recording"], true);

    // Real keyboard input: type a name into the entry, then activate the button with Return.
    focus(&c, "name-entry").await;
    let mut input = RealInput::start(&bin, &display);
    input.call("type_text", json!({"text": "alice"}));
    until("entry text arrived", Duration::from_secs(10), async || (text_of(&c, "name-entry").await == "alice").then_some(())).await;
    tokio::time::sleep(Duration::from_millis(500)).await; // past the 300 ms window
    let w = c.json("wait_for", json!({"target": by_id("submit"), "condition": "sensitive", "timeout_ms": 5000})).await;
    assert_eq!(w["satisfied"], true, "{w}");
    focus(&c, "submit").await;
    input.call("key", json!({"keys": ["Return"]}));
    until("click counted", Duration::from_secs(10), async || (text_of(&c, "click-counter").await == "clicks: 1").then_some(())).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let s = c.json("stop_recording", json!({})).await;
    let steps = s["batch"]["steps"].as_array().unwrap().clone();
    assert_eq!(steps.len(), 2, "one fill (coalesced) and one activate: {s}");
    assert_eq!(steps[0]["tool"], "interact");
    assert_eq!(steps[0]["arguments"], json!({"target": by_id("name-entry"), "operation": "fill", "text": "alice"}));
    assert_eq!(steps[1]["arguments"], json!({"target": by_id("submit"), "operation": "activate"}));
    assert!(s["human_emissions"].as_u64().unwrap() >= 6, "five keystrokes and a click: {s}");
    assert_eq!(s["batch"]["stop_on_error"], true);
    drop(input);
    drop(c);
    drop(fx);

    // Replay in a new session reproduces the state.
    let fx2 = Fixture::start(&display, &[]);
    let (c2, _b2) = client(&fx2);
    let r = c2.json("batch", json!({"steps": steps})).await;
    assert!(r["stopped_at"].is_null(), "{r}");
    assert_eq!(text_of(&c2, "name-entry").await, "alice");
    assert_eq!(text_of(&c2, "click-counter").await, "clicks: 1");
}

/// gm-oa8: a key controller listed while its entry was not secret becomes
/// secret with the entry (input-purpose switched to password): a signal watch
/// on it reports names only, also for real key presses.
#[tokio::test(flavor = "multi_thread")]
async fn controller_of_an_entry_that_became_secret_is_secret() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let policy = Policy::default();
    let c = Client { broker: Broker::new(policy, Config::with_socket_dir(fx.dir.path())).unwrap() };
    let q = c.json("query", json!({"selector": {"type_name": "GtkText", "ancestor": by_id("name-entry")}})).await;
    let text = q["matches"][0]["ref"].as_str().unwrap_or_else(|| panic!("{q}")).to_owned();
    let r = c.json("inspect_controllers", json!({"target": text})).await;
    let key = r["controllers"].as_array().unwrap().iter().find(|x| x["type"] == "GtkEventControllerKey").unwrap_or_else(|| panic!("{r}"));
    assert!(key.get("secret").is_none() || key["secret"] == false, "not secret yet: {key}");
    let key = key["ref"].as_str().unwrap().to_owned();

    c.json("set_property", json!({"target": by_id("name-entry"), "name": "input-purpose", "value": "password"})).await;
    let w = c.json("watch", json!({"target": key, "kind": "signal", "signal": "key-pressed"})).await;
    assert_eq!(w["secret"], true, "{w}");

    let Some(bin) = wayland_mcp_binary() else {
        eprintln!("controller_of_an_entry_that_became_secret_is_secret: real key part SKIPPED, wayland-mcp binary not found");
        return;
    };
    focus(&c, "name-entry").await;
    let mut input = RealInput::start(&bin, &display);
    input.call("key", json!({"keys": ["Left"]}));
    let ev = wait_event(&c, "key-pressed on the secret entry", |e| e["signal"] == "key-pressed").await;
    assert_eq!(ev["secret"], true, "{ev}");
    assert!(ev.get("args").is_none(), "no keyval of a secret field: {ev}");
}

#[tokio::test(flavor = "multi_thread")]
async fn record_real_pointer_click_and_secret_typing() {
    let display = require_display!();
    let Some(bin) = wayland_mcp_binary() else {
        eprintln!("record_real_pointer_*: SKIPPED, wayland-mcp binary not found");
        return;
    };
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    c.settle_tree().await;
    c.json("record", json!({})).await;

    // Secret typed with real keys becomes a placeholder.
    focus(&c, "password").await;
    let mut input = RealInput::start(&bin, &display);
    input.call("type_text", json!({"text": SECRET}));
    until("password typed", Duration::from_secs(10), async || {
        let v = c.json("get_property", json!({"target": by_id("password"), "names": ["text"]})).await;
        (v["values"]["text"] == json!({"redacted": true})).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A real pointer click on a button: the window sits at (16, 16) of the compositor's output.
    let snap = c.snapshot_json(json!({})).await;
    let node = node_by_name(&snap, "save-primary").clone();
    let b = &node["bounds_in_root"];
    assert!(b.is_object(), "bounds_in_root expected: {node}");
    let (x, y) = (b["x"].as_f64().unwrap() + b["width"].as_f64().unwrap() / 2.0, b["y"].as_f64().unwrap() + b["height"].as_f64().unwrap() / 2.0);
    drop(input);
    let mut input = RealInput::start(&bin, &display); // a fresh server per pointer click
    input.call("click", json!({"x": 16.0 + x, "y": 16.0 + y, "space": "logical"}));
    until("pointer click counted", Duration::from_secs(10), async || (text_of(&c, "click-counter").await == "clicks: 1").then_some(())).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let s = c.json("stop_recording", json!({})).await;
    let raw = serde_json::to_string(&s).unwrap();
    assert!(!raw.contains(SECRET), "the typed secret must not appear anywhere: {raw}");
    let steps = s["batch"]["steps"].as_array().unwrap();
    assert_eq!(steps[0]["arguments"], json!({"target": by_id("password"), "operation": "fill", "text": "{{secret:1}}"}), "{s}");
    assert_eq!(steps[1]["arguments"]["operation"], "activate");
    assert_eq!(steps[1]["arguments"]["target"], by_id("save-primary"));
    assert_eq!(s["secrets"][0]["target"], by_id("password"));
}

#[tokio::test(flavor = "multi_thread")]
async fn record_popover_menu_activation_is_attributed_to_the_human() {
    let display = require_display!();
    let Some(bin) = wayland_mcp_binary() else {
        eprintln!("record_popover_*: SKIPPED, wayland-mcp binary not found");
        return;
    };
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = client(&fx);
    c.settle_tree().await;
    c.json("record", json!({})).await;
    // The agent opens the popover (ignored); the user picks the first item with the keyboard.
    c.json("interact", json!({"target": by_id("menu-button"), "operation": "popup"})).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut input = RealInput::start(&bin, &display);
    input.call("key", json!({"keys": ["Return"]})); // the first item is focused when the popover opens
    until("menu item activated", Duration::from_secs(10), async || (text_of(&c, "click-counter").await == "clicks: 1").then_some(())).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let s = c.json("stop_recording", json!({})).await;
    let steps = s["batch"]["steps"].as_array().unwrap();
    let activate: Vec<&Value> = steps.iter().filter(|st| st["tool"] == "activate_action").collect();
    assert_eq!(activate.len(), 1, "{s}");
    assert_eq!(activate[0]["arguments"]["name"], "win.save");
    // everything else is the menu button's popover closing
    for st in steps.iter().filter(|st| st["tool"] != "activate_action") {
        assert_eq!(st["arguments"]["target"], by_id("menu-button"), "{s}");
        assert_eq!(st["arguments"]["operation"], "popdown");
    }
    assert!(s["app_emissions"].as_array().unwrap().is_empty(), "{s}");
}
