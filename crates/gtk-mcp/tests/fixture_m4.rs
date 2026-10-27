//! M4 acceptance: AC-11 (launch, post-mortem, refusals, env rejection) plus the
//! app toolset, `object_stats` and `register_state` resources, against the real
//! fixture through the real broker. Every wait is bounded; the harness watchdog
//! turns any hang into a diagnosable failure.

mod fixture_env;

use base64::Engine;
use fixture_env::*;
use gtk_mcp::launch::{self, Launch, LaunchSpec};
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::{ErrorCode, LaunchAppParams};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const SCHEMA_ID: &str = "dev.gtkmcp.Fixture.Prefs";
const SCHEMA_XML: &str = r#"<schemalist>
  <schema id="dev.gtkmcp.Fixture.Prefs" path="/dev/gtkmcp/fixture/prefs/">
    <key name="volume" type="i"><default>5</default><range min="0" max="10"/>
      <summary>Volume</summary><description>Output volume</description></key>
    <key name="greeting" type="s"><default>'hello'</default><summary>Greeting</summary></key>
    <key name="enabled" type="b"><default>false</default></key>
    <key name="palette" type="as"><default>['red','green']</default></key>
    <key name="ratio" type="d"><default>0.5</default></key>
    <key name="api-token" type="s"><default>''</default></key>
  </schema>
  <schema id="dev.gtkmcp.Elsewhere" path="/dev/gtkmcp/elsewhere/">
    <key name="knob" type="i"><default>1</default></key>
  </schema>
  <schema id="dev.gtkmcp.Fixture.Profile">
    <key name="knob" type="i"><default>1</default></key>
  </schema>
</schemalist>"#;
const RES_XML: &str = r#"<gresources><gresource prefix="/dev/gtkmcp/fixture">
  <file compressed="true">hello.txt</file><file>data/blob.bin</file></gresource></gresources>"#;
const HELLO: &str = "hello from the fixture resource\nsecond line — ünïcode\n";
const BLOB: [u8; 6] = [0, 1, 2, 255, 254, 128];
const TOKEN: &str = "hunter2-TOKEN-0xC0FFEE";

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

/// Compiled test schema and GResource in a temp dir (`glib-compile-*`).
struct Assets {
    _dir: tempfile::TempDir,
    schema_dir: PathBuf,
    gresource: PathBuf,
}

fn assets() -> Assets {
    let dir = tempfile::tempdir().unwrap();
    let schema_dir = dir.path().join("schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    std::fs::write(schema_dir.join("dev.gtkmcp.Fixture.gschema.xml"), SCHEMA_XML).unwrap();
    let st = Command::new("glib-compile-schemas").arg(&schema_dir).status().expect("glib-compile-schemas");
    assert!(st.success(), "glib-compile-schemas failed");
    let res = dir.path().join("res");
    std::fs::create_dir_all(res.join("data")).unwrap();
    std::fs::write(res.join("hello.txt"), HELLO).unwrap();
    std::fs::write(res.join("data/blob.bin"), BLOB).unwrap();
    std::fs::write(res.join("fixture.gresource.xml"), RES_XML).unwrap();
    let gresource = dir.path().join("fixture.gresource");
    let st = Command::new("glib-compile-resources")
        .arg(format!("--sourcedir={}", res.display()))
        .arg(format!("--target={}", gresource.display()))
        .arg(res.join("fixture.gresource.xml"))
        .status()
        .expect("glib-compile-resources");
    assert!(st.success(), "glib-compile-resources failed");
    Assets { _dir: dir, schema_dir, gresource }
}

impl Assets {
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("GSETTINGS_SCHEMA_DIR", self.schema_dir.to_string_lossy().into_owned()),
            ("FIXTURE_GRESOURCE", self.gresource.to_string_lossy().into_owned()),
        ]
    }
}

/// Broker with launch enabled, socket dir = `dir`.
fn launcher(dir: &Path, allow_launch: bool) -> (Client, Arc<Broker>) {
    let policy = Policy { toolsets: vec!["core".into(), "app".into(), "diagnostics".into()], allow_launch, ..Policy::default() };
    let broker = Broker::new(policy, Config::with_socket_dir(dir)).unwrap();
    (Client { broker: broker.clone() }, broker)
}

fn fixture_argv(extra: &[&str]) -> Vec<String> {
    let mut v = vec![fixture_binary().to_string_lossy().into_owned()];
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

/// `launch_app` arguments for the fixture on the test display.
fn launch_json(d: &Display, a: &Assets, argv: Vec<String>) -> Value {
    let mut env = serde_json::Map::new();
    for (k, v) in a.env() {
        env.insert(k.into(), json!(v));
    }
    env.insert("GSETTINGS_BACKEND".into(), json!("memory"));
    env.insert("GDK_BACKEND".into(), json!("wayland"));
    json!({
        "argv": argv,
        "env": env,
        "presets": {"a11y": "none"},
        "wayland_display": d.wayland_display,
        "xdg_runtime_dir": d.xdg_runtime_dir,
    })
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

async fn exited_entry(c: &Client, session: &str) -> Value {
    until("session exited in list_apps", Duration::from_secs(10), async || {
        let apps = c.json("list_apps", json!({})).await;
        apps.as_array()?.iter().find(|a| a["session"] == session && a["state"].get("exited").is_some()).cloned()
    })
    .await
}

fn pid_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return true;
    }
    // a zombie that init has not reaped yet is gone for our purposes
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
}

fn pgrp_of(pid: i32) -> i32 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    s.rsplit(") ").next().unwrap().split_whitespace().nth(2).unwrap().parse().unwrap()
}

fn logs_text(b: &Broker, session: &str) -> String {
    b.session(session).unwrap().logs_since(0).entries.iter().filter_map(|e| e.value["text"].as_str()).collect::<Vec<_>>().join("\n")
}

// ---- AC-11: launch ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_launch_appears_running_and_captures_output() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, broker) = launcher(dir.path(), true);
    let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&["--banner=hello-m4"]))).await;
    assert_eq!(r["state"], "running", "{r}");
    assert_eq!(r["launched_by_broker"], true);
    assert_eq!(r["hello"]["app_id"], "dev.gtkmcp.Fixture");
    let sid = r["session"].as_str().unwrap().to_owned();
    let pid = r["pid"].as_u64().unwrap();
    assert_eq!(r["launch"]["pid"].as_u64(), Some(pid));

    let apps = c.json("list_apps", json!({})).await;
    assert_eq!(apps.as_array().unwrap().len(), 1);
    assert_eq!(apps[0]["launched_by_broker"], true);
    assert_eq!(apps[0]["state"], "running");

    // stdout and stderr are captured in the session's cursor log
    let s = broker.session(&sid).unwrap();
    let lines = until("banner in captured logs", Duration::from_secs(10), async || {
        let l = s.logs_since(0);
        let has = |src: &str| l.entries.iter().any(|e| e.value["source"] == src && e.value["text"] == "fixture-banner: hello-m4");
        (has("stdout") && has("stderr")).then_some(l)
    })
    .await;
    let n = lines.next_cursor;
    assert!(s.logs_since(n).entries.is_empty(), "cursor resumes after the last entry");

    // the app sees only GTK_MCP_DIR (set by the broker, not scrubbed) and works through the broker
    let env = c.json("inspect", json!({"target": by_id("env-report")})).await;
    assert_eq!(env["text"], "gtk-mcp-env: GTK_MCP_DIR");
    broker.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_launched_app_mutates_without_any_flag() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, _b) = launcher(dir.path(), true);
    let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&[]))).await;
    assert_eq!(r["state"], "running", "{r}");
    assert!(r["hello"].get("mutations_enabled").is_none(), "handshake carries no mutation flag: {r}");
    let r = c.json("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert!(r["observed"].is_object(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_launch_disabled_without_allow_launch_spawns_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let (c, _b) = launcher(dir.path(), false);
    let argv = vec!["sh".to_owned(), "-c".to_owned(), format!("touch {}", marker.display())];
    let e = c.err("launch_app", json!({"argv": argv})).await;
    assert_eq!(e.code, ErrorCode::LaunchDisabled);
    for tool in ["restart_app", "stop_app"] {
        assert_eq!(c.err(tool, json!({})).await.code, ErrorCode::LaunchDisabled, "{tool}");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!marker.exists(), "no process may be spawned without --allow-launch");
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_env_with_gtk_mcp_variables_passes_through() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, broker) = launcher(dir.path(), true);
    let mut args = launch_json(&display, &a, fixture_argv(&[]));
    args["env"]["GTK_MCP_X"] = json!("1");
    let r = c.json("launch_app", args).await;
    assert_eq!(r["state"], "running", "{r}");
    let env = c.json("inspect", json!({"target": by_id("env-report")})).await;
    assert!(env["text"].as_str().unwrap().contains("GTK_MCP_X"), "{}", env["text"]);
    broker.shutdown();

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let (c, _b) = launcher(dir.path(), true);
    for bad in [json!({"argv": []}), json!({"argv": ["sh"], "cwd": "/nonexistent-dir-xyz"}), json!({"argv": ["/nonexistent/program-xyz"]})] {
        assert_eq!(c.err("launch_app", bad.clone()).await.code, ErrorCode::InvalidParams, "{bad}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!marker.exists());
    assert!(c.json("list_apps", json!({})).await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_app_that_exits_before_attaching_reports_code_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let (c, _b) = launcher(dir.path(), true);
    let e = c.err("launch_app", json!({"argv": ["sh", "-c", "echo boom-line >&2; exit 3"], "timeout_ms": 5000})).await;
    assert_eq!(e.code, ErrorCode::SessionExited);
    let d = e.data.unwrap();
    assert_eq!(d["code"], 3);
    assert!(d["logs"].as_array().unwrap().iter().any(|l| l["text"] == "boom-line" && l["source"] == "stderr"), "{d}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_launch_timeout_stops_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let (c, _b) = launcher(dir.path(), true);
    let script = format!("echo $$ > {}; exec sleep 30", pidfile.display());
    let e = c.err("launch_app", json!({"argv": ["sh", "-c", script], "timeout_ms": 800})).await;
    assert_eq!(e.code, ErrorCode::Timeout);
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    until("timed-out launch killed", Duration::from_secs(5), async || pid_gone(pid).then_some(())).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_stop_escalates_to_sigkill_for_apps_ignoring_sigterm() {
    let params: LaunchAppParams = serde_json::from_value(json!({
        "argv": ["sh", "-c", "trap '' TERM; echo ready; while :; do sleep 1; done"]
    }))
    .unwrap();
    let spec = LaunchSpec::from_params(params).unwrap();
    let plan = spec.env_plan(Path::new("/tmp"));
    let l: Arc<Launch> = Launch::spawn(spec, &plan, 64).unwrap();
    until("child ready", Duration::from_secs(5), async || l.log_tail(5).iter().any(|e| e["text"] == "ready").then_some(())).await;
    let t = Instant::now();
    let forced = launch::stop(&l, Duration::from_millis(400)).await;
    assert!(forced, "SIGTERM is ignored: SIGKILL needed");
    assert!(t.elapsed() >= Duration::from_millis(400));
    let exit = l.wait_exit(Duration::from_secs(3)).expect("exit status");
    assert_eq!(exit.signal, Some(libc::SIGKILL));
    assert!(!l.group_alive());
    // a cooperative child needs no SIGKILL
    let spec = LaunchSpec::from_params(serde_json::from_value(json!({"argv": ["sleep", "30"]})).unwrap()).unwrap();
    let l = Launch::spawn(spec, &plan, 64).unwrap();
    assert!(!launch::stop(&l, Duration::from_secs(3)).await);
    assert_eq!(l.wait_exit(Duration::from_secs(3)).unwrap().signal, Some(libc::SIGTERM));
}

// ---- AC-11: post-mortem ----------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_panic_exits_with_signal_in_flight_id_and_last_logs() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, broker) = launcher(dir.path(), true);
    let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&[]))).await;
    let sid = r["session"].as_str().unwrap().to_owned();

    // the panic happens inside the `interact activate` request: it is the in-flight one
    let e = c.err("interact", json!({"target": by_id("crash-panic"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::SessionExited, "{e:?}");
    let in_flight = e.data.as_ref().unwrap()["in_flight"].as_u64().expect("in-flight id in the error");

    let app = exited_entry(&c, &sid).await;
    let ex = &app["state"]["exited"];
    assert_eq!(ex["signal"], libc::SIGABRT, "{app}");
    assert_eq!(ex["in_flight"].as_u64(), Some(in_flight));
    assert_eq!(app["launched_by_broker"], true);
    let last = app["post_mortem"]["logs"]["last"].as_array().unwrap();
    assert!(!last.is_empty() && last.len() <= 20, "last logs: {last:?}");
    assert!(logs_text(&broker, &sid).contains("fixture crash hook: panic in a GTK callback"), "the panic message is retained");
    assert!(app["post_mortem"]["logs"]["retained"].as_u64().unwrap() >= last.len() as u64);
    // the library API keeps working on the retained session
    assert!(logs_text(&broker, &sid).contains("fixture-crash: panicking"));
    // and further calls say why they fail
    let e = c.err("list_windows", json!({"session": sid})).await;
    assert_eq!(e.code, ErrorCode::SessionExited);
    assert_eq!(e.data.unwrap()["signal"], libc::SIGABRT);
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_sigsegv_outside_a_request_has_no_in_flight_id() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, _b) = launcher(dir.path(), true);
    let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&[]))).await;
    let sid = r["session"].as_str().unwrap().to_owned();
    c.json("interact", json!({"target": by_id("crash-segv-delayed"), "operation": "activate"})).await;
    let app = exited_entry(&c, &sid).await;
    let ex = &app["state"]["exited"];
    assert_eq!(ex["signal"], libc::SIGSEGV, "{app}");
    assert!(ex["in_flight"].is_null());
    assert!(app["post_mortem"]["logs"]["last"].as_array().unwrap().iter().any(|l| l["text"] == "fixture-crash: raising SIGSEGV"));
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_exited_sessions_are_bounded_by_retain_exited() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let policy = Policy { allow_launch: true, ..Policy::default() };
    let config = Config { retain_exited: 1, ..Config::with_socket_dir(dir.path()) };
    let c = Client { broker: Broker::new(policy, config).unwrap() };
    let mut ids = Vec::new();
    for _ in 0..2 {
        let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&[]))).await;
        let sid = r["session"].as_str().unwrap().to_owned();
        c.json("stop_app", json!({"session": sid})).await;
        ids.push(sid);
    }
    let apps = c.json("list_apps", json!({})).await;
    let listed: Vec<&str> = apps.as_array().unwrap().iter().map(|a| a["session"].as_str().unwrap()).collect();
    assert_eq!(listed, vec![ids[1].as_str()], "only the newest exited session is retained");
}

// ---- AC-11: restart / stop -------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_restart_and_stop_refuse_processes_not_launched_by_the_broker() {
    let display = require_display!();
    let mut fx = Fixture::start(&display, &[]);
    let (c, _b) = launcher(fx.dir.path(), true);
    let apps = c.json("list_apps", json!({})).await;
    assert_eq!(apps[0]["launched_by_broker"], false);
    for tool in ["restart_app", "stop_app"] {
        let e = c.err(tool, json!({"session": "s1"})).await;
        assert_eq!(e.code, ErrorCode::UnsupportedOperation, "{tool}: {e}");
        assert!(e.message.contains("launched"), "{}", e.message);
    }
    assert!(fx.child.try_wait().unwrap().is_none(), "the external process must still be running");
    c.json("list_windows", json!({})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_stop_terminates_the_whole_process_group() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("sleeper.pid");
    let (c, broker) = launcher(dir.path(), true);
    // a background child of the launched shell, then exec into the app: same process group
    let script = format!("sleep 300 & echo $! > {}; exec \"$0\" \"$@\"", pidfile.display());
    let mut argv = vec!["sh".to_owned(), "-c".to_owned(), script];
    argv.extend(fixture_argv(&[]));
    let r = c.json("launch_app", launch_json(&display, &a, argv)).await;
    let sid = r["session"].as_str().unwrap().to_owned();
    let pid = r["pid"].as_i64().unwrap() as i32;
    let sleeper: i32 = until("sleeper pid file", Duration::from_secs(5), async || std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()).await;
    assert_eq!(pgrp_of(pid), pid, "the app leads its own process group");
    assert_eq!(pgrp_of(sleeper), pid, "its child shares the group");

    let r = c.json("stop_app", json!({"session": sid})).await;
    assert_eq!(r["forced_kill"], false, "{r}");
    assert_eq!(r["already_exited"], false);
    assert_eq!(r["exit"]["signal"], libc::SIGTERM);
    assert!(pid_gone(pid));
    until("group member killed", Duration::from_secs(5), async || pid_gone(sleeper).then_some(())).await;
    let apps = c.json("list_apps", json!({})).await;
    assert_eq!(apps[0]["state"]["exited"]["signal"], libc::SIGTERM);
    // stopping again is not an error
    let r = c.json("stop_app", json!({"session": sid})).await;
    assert_eq!(r["already_exited"], true);
    broker.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_restart_yields_a_new_session_and_the_old_one_is_exited() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, _b) = launcher(dir.path(), true);
    let r = c.json("launch_app", launch_json(&display, &a, fixture_argv(&["--banner=restart-me"]))).await;
    let old = r["session"].as_str().unwrap().to_owned();
    let old_pid = r["pid"].as_i64().unwrap() as i32;
    let snap = c.snapshot_json(json!({})).await;
    let old_ref = node_by_name(&snap, "save-primary")["ref"].as_str().unwrap().to_owned();

    let r = c.json("restart_app", json!({"session": old})).await;
    let new = r["session"]["session"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    assert_eq!(r["session"]["state"], "running");
    assert_eq!(r["session"]["launched_by_broker"], true);
    assert_ne!(r["session"]["pid"].as_i64().unwrap() as i32, old_pid);
    assert_eq!(r["previous"]["exit"]["signal"], libc::SIGTERM);
    assert!(pid_gone(old_pid));

    // old refs belong to the old session, which is gone
    let e = c.err("inspect", json!({"session": old, "target": old_ref})).await;
    assert_eq!(e.code, ErrorCode::SessionExited);
    // the same launch spec ran again: the banner is in the new session's log
    let apps = c.json("list_apps", json!({})).await;
    assert_eq!(apps.as_array().unwrap().len(), 2);
    let new_app = apps.as_array().unwrap().iter().find(|a| a["session"] == new).unwrap();
    assert_eq!(new_app["state"], "running");
    // the new app answers on its own session
    let s = c.snapshot_json(json!({"session": new})).await;
    node_by_name(&s, "save-primary");
    // restart also revives a crashed app
    let e = c.err("interact", json!({"session": new, "target": by_id("crash-panic"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::SessionExited);
    let r = c.json("restart_app", json!({"session": new})).await;
    assert_eq!(r["session"]["state"], "running");
    assert_eq!(r["previous"]["exit"]["signal"], libc::SIGABRT);
}

// ---- app_open ----------------------------------------------------------------

fn app_broker(fx: &Fixture) -> (Client, Arc<Broker>) {
    let policy = Policy { toolsets: vec!["core".into(), "app".into(), "diagnostics".into()], ..Policy::default() };
    let b = Broker::new(policy, Config::with_socket_dir(fx.dir.path())).unwrap();
    (Client { broker: b.clone() }, b)
}

async fn text_of(c: &Client, id: &str) -> String {
    c.json("inspect", json!({"target": by_id(id)})).await["text"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn app_open_activates_and_opens_files() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, _b) = app_broker(&fx);
    assert_eq!(text_of(&c, "activate-report").await, "activations: 1");
    let r = c.json("app_open", json!({"activate": true})).await;
    assert_eq!(r["action"], "activate");
    assert_eq!(text_of(&c, "activate-report").await, "activations: 2");
    let r = c.json("app_open", json!({})).await;
    assert_eq!(r["action"], "activate");
    assert_eq!(text_of(&c, "activate-report").await, "activations: 3");

    let r = c.json("app_open", json!({"files": ["/tmp/m4-a.txt", "file:///tmp/m4-b.txt"], "hint": "edit"})).await;
    assert_eq!(r["action"], "open");
    assert_eq!(r["files"], json!(["file:///tmp/m4-a.txt", "file:///tmp/m4-b.txt"]));
    assert_eq!(text_of(&c, "open-report").await, "open: file:///tmp/m4-a.txt,file:///tmp/m4-b.txt");
    assert!(fx.stderr().contains("fixture-open: 2 file(s), hint 'edit'"), "{}", fx.stderr());
    // activation did not run for the open
    assert_eq!(text_of(&c, "activate-report").await, "activations: 3");

    let e = c.err("app_open", json!({"files": ["/tmp/x"], "activate": true})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("app_open", json!({"activate": false})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
}

// ---- GSettings -----------------------------------------------------------------

fn find_key<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema["keys"].as_array().unwrap().iter().find(|k| k["name"] == name).unwrap_or_else(|| panic!("no key {name} in {schema}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn gsettings_inspect_set_and_read_back() {
    let display = require_display!();
    let a = assets();
    let fx = Fixture::start(&display, &a.env().iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>());
    let (c, _b) = app_broker(&fx);

    let r = c.json("inspect_gsettings", json!({})).await;
    let schema = r["schemas"].as_array().unwrap().iter().find(|s| s["id"] == SCHEMA_ID).unwrap_or_else(|| panic!("{r}"));
    assert_eq!(schema["source"], "registered", "the settings object the app registered is reported");
    assert_eq!(schema["path"], "/dev/gtkmcp/fixture/prefs/");
    let vol = find_key(schema, "volume");
    assert_eq!((vol["type"].as_str(), &vol["value"], &vol["default"]), (Some("i"), &json!(5), &json!(5)));
    assert_eq!(vol["user_set"], false);
    assert_eq!(vol["writable"], true);
    assert_eq!(vol["range"], json!({"kind": "range", "data": [0, 10]}));
    assert_eq!(vol["summary"], "Volume");
    assert_eq!(find_key(schema, "palette")["value"], json!(["red", "green"]));
    assert_eq!(text_of(&c, "gsettings-report").await, "gsettings: greeting=hello volume=5");

    let r = c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "volume", "value": 7})).await;
    assert_eq!((&r["previous"], &r["value"], &r["changed"]), (&json!(5), &json!(7), &json!(true)), "{r}");
    let r = c.json("inspect_gsettings", json!({"schema": SCHEMA_ID})).await;
    assert_eq!(find_key(&r["schemas"][0], "volume")["value"], 7);
    assert_eq!(find_key(&r["schemas"][0], "volume")["user_set"], true);
    // the app's own GSettings object saw the change
    until("app observed the write", Duration::from_secs(5), async || (text_of(&c, "gsettings-report").await == "gsettings: greeting=hello volume=7").then_some(())).await;
    assert!(fx.stderr().contains("fixture-gsettings-changed: volume"));

    // string, bool, double, array (JSON and GVariant text)
    let r = c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "greeting", "value": "héllo"})).await;
    assert_eq!(r["value"], "héllo");
    assert_eq!(c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "enabled", "value": true})).await["value"], true);
    assert_eq!(c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "ratio", "value": 0.25})).await["value"], 0.25);
    assert_eq!(c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "palette", "value": ["a", "b"]})).await["value"], json!(["a", "b"]));
    let r = c.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "palette", "value_text": "['x']"})).await;
    assert_eq!(r["value"], json!(["x"]));

    // validation: type, range, key, schema, arguments; nothing changes
    for (key, value, code) in [
        ("volume", json!(99), ErrorCode::InvalidValue),
        ("volume", json!(-1), ErrorCode::InvalidValue),
        ("volume", json!("loud"), ErrorCode::InvalidValue),
        ("volume", json!(1.5), ErrorCode::InvalidValue),
        ("greeting", json!(5), ErrorCode::InvalidValue),
        ("palette", json!([1]), ErrorCode::InvalidValue),
        ("nope", json!(1), ErrorCode::NotFound),
    ] {
        let e = c.err("set_gsetting", json!({"schema": SCHEMA_ID, "key": key, "value": value})).await;
        assert_eq!(e.code, code, "{key} = {value}: {e}");
    }
    let e = c.err("set_gsetting", json!({"schema": SCHEMA_ID, "key": "volume", "value_text": "'x'"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("set_gsetting", json!({"schema": "dev.nope.Missing", "key": "k", "value": 1})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("set_gsetting", json!({"schema": SCHEMA_ID, "key": "volume"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("set_gsetting", json!({"schema": SCHEMA_ID, "key": "volume", "value": 1, "value_text": "1"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert_eq!(c.err("inspect_gsettings", json!({"schema": "dev.nope.Missing"})).await.code, ErrorCode::NotFound);
    let r = c.json("inspect_gsettings", json!({"schema": SCHEMA_ID})).await;
    assert_eq!(find_key(&r["schemas"][0], "volume")["value"], 7);

    // key names that look like credentials are redacted in every response
    let out = c.call("set_gsetting", json!({"schema": SCHEMA_ID, "key": "api-token", "value": TOKEN})).await.unwrap();
    assert!(!out.text_content().contains(TOKEN), "{}", out.text_content());
    assert!(out.text_content().contains("redacted"));
    let out = c.call("inspect_gsettings", json!({})).await.unwrap();
    assert!(!out.text_content().contains(TOKEN));
    assert_eq!(find_key(&out.json_content().unwrap()["schemas"][0], "api-token")["value"], json!({"redacted": true}));
    // a rejected write never echoes the offered value
    let e = c.err("set_gsetting", json!({"schema": SCHEMA_ID, "key": "api-token", "value": [TOKEN]})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    assert!(!format!("{e:?}").contains(TOKEN), "{e:?}");

    // schemas outside the app (registered objects and its application id) are read-only
    let r = c.json("inspect_gsettings", json!({"schema": "dev.gtkmcp.Elsewhere"})).await;
    assert_eq!(find_key(&r["schemas"][0], "knob")["value"], 1);
    let e = c.err("set_gsetting", json!({"schema": "dev.gtkmcp.Elsewhere", "key": "knob", "value": 2})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation, "{e:?}");
    let r = c.json("inspect_gsettings", json!({"schema": "dev.gtkmcp.Elsewhere"})).await;
    assert_eq!(find_key(&r["schemas"][0], "knob")["value"], 1, "nothing was written");

    // gm-m7c: the app's relocatable schema is written under the app's own path
    // only, never at another schema's location in the shared backend.
    const PROFILE: &str = "dev.gtkmcp.Fixture.Profile";
    let e = c.err("set_gsetting", json!({"schema": PROFILE, "path": "/dev/gtkmcp/elsewhere/", "key": "knob", "value": 2})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation, "{e:?}");
    let r = c.json("inspect_gsettings", json!({"schema": "dev.gtkmcp.Elsewhere"})).await;
    assert_eq!(find_key(&r["schemas"][0], "knob")["value"], 1, "nothing was written through the relocatable schema");
    let r = c.json("set_gsetting", json!({"schema": PROFILE, "path": "/dev/gtkmcp/fixture/profiles/a/", "key": "knob", "value": 3})).await;
    assert_eq!(r["value"], 3, "{r}");
    let e = c.err("set_gsetting", json!({"schema": PROFILE, "key": "knob", "value": 3})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams, "a relocatable schema needs a path");

    // a second broker over the same session reads and writes without any flag
    let (ro, _b2) = app_broker(&fx);
    assert_eq!(find_key(&ro.json("inspect_gsettings", json!({"schema": SCHEMA_ID})).await["schemas"][0], "volume")["value"], 7);
    let r = ro.json("set_gsetting", json!({"schema": SCHEMA_ID, "key": "volume", "value": 1})).await;
    assert_eq!(r["value"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn gsettings_schemas_under_the_app_id_are_found_without_registration() {
    let display = require_display!();
    let a = assets();
    // GSETTINGS_SCHEMA_DIR unset for the app => nothing under its id; the explicit lookup fails clearly
    let fx = Fixture::start(&display, &[("FIXTURE_GRESOURCE", a.gresource.to_str().unwrap())]);
    let (c, _b) = app_broker(&fx);
    let r = c.json("inspect_gsettings", json!({})).await;
    assert!(r["schemas"].as_array().unwrap().iter().all(|s| s["id"] != SCHEMA_ID), "{r}");
    assert_eq!(c.err("inspect_gsettings", json!({"schema": SCHEMA_ID})).await.code, ErrorCode::NotFound);
    drop(fx);
    // with the schema installed but only found by id prefix (the agent knows no settings object):
    // the fixture registers its object, so the prefix path is covered by a second schema source entry
    let fx = Fixture::start(&display, &a.env().iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>());
    let (c, _b) = app_broker(&fx);
    let r = c.json("inspect_gsettings", json!({})).await;
    assert_eq!(r["app_id"], "dev.gtkmcp.Fixture");
    assert_eq!(r["schemas"].as_array().unwrap().iter().filter(|s| s["id"] == SCHEMA_ID).count(), 1, "registered and prefix match are reported once");
}

// ---- resources -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn resources_are_listed_and_read_with_size_limits() {
    let display = require_display!();
    let a = assets();
    let fx = Fixture::start(&display, &a.env().iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>());
    let (c, _b) = app_broker(&fx);

    let r = c.json("list_resources", json!({})).await;
    let top: Vec<&str> = r["resources"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert!(top.contains(&"/dev/"), "{top:?}");
    let r = c.json("list_resources", json!({"path": "/dev/gtkmcp/fixture/"})).await;
    let paths: Vec<(&str, &str)> = r["resources"].as_array().unwrap().iter().map(|e| (e["path"].as_str().unwrap(), e["kind"].as_str().unwrap())).collect();
    assert_eq!(paths, vec![("/dev/gtkmcp/fixture/data/", "directory"), ("/dev/gtkmcp/fixture/hello.txt", "file")]);
    let hello = &r["resources"][1];
    assert_eq!(hello["size"], HELLO.len());
    assert_eq!(hello["compressed"], true);
    let r = c.json("list_resources", json!({"path": "/dev/", "recursive": true})).await;
    let files: Vec<&str> = r["resources"].as_array().unwrap().iter().filter(|e| e["kind"] == "file").map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(files, vec!["/dev/gtkmcp/fixture/hello.txt", "/dev/gtkmcp/fixture/data/blob.bin"]);
    let r = c.json("list_resources", json!({"path": "/dev/", "recursive": true, "limit": 2})).await;
    assert_eq!(r["resources"].as_array().unwrap().len(), 2);
    assert_eq!(r["truncated"], true);
    assert_eq!(c.err("list_resources", json!({"path": "/dev/nope/"})).await.code, ErrorCode::NotFound);
    assert_eq!(c.err("list_resources", json!({"path": "relative/"})).await.code, ErrorCode::InvalidParams);

    let r = c.json("read_resource", json!({"path": "/dev/gtkmcp/fixture/hello.txt"})).await;
    assert_eq!(r["content"], HELLO);
    assert_eq!((&r["encoding"], &r["truncated"], &r["size"]), (&json!("utf8"), &json!(false), &json!(HELLO.len())));
    assert_eq!(r["content_type"], "text/plain");
    // truncation never splits a multi-byte character into "binary"
    let cut = HELLO.find('—').unwrap() + 1;
    let r = c.json("read_resource", json!({"path": "/dev/gtkmcp/fixture/hello.txt", "max_bytes": cut})).await;
    assert_eq!(r["truncated"], true);
    assert_eq!(r["encoding"], "utf8");
    assert_eq!(r["content"], &HELLO[..cut - 1]);
    let r = c.json("read_resource", json!({"path": "/dev/gtkmcp/fixture/data/blob.bin"})).await;
    assert_eq!(r["encoding"], "base64");
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(r["content"].as_str().unwrap()).unwrap(), BLOB);
    let r = c.json("read_resource", json!({"path": "/dev/gtkmcp/fixture/data/blob.bin", "max_bytes": 3})).await;
    assert_eq!((r["truncated"].as_bool(), r["returned"].as_u64(), r["size"].as_u64()), (Some(true), Some(3), Some(6)));
    let e = c.err("read_resource", json!({"path": "/dev/gtkmcp/fixture/data/blob.bin", "encoding": "text"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    assert_eq!(c.err("read_resource", json!({"path": "/dev/gtkmcp/fixture/missing"})).await.code, ErrorCode::NotFound);
    assert_eq!(c.err("read_resource", json!({"path": "/dev/gtkmcp/fixture/hello.txt", "max_bytes": 0})).await.code, ErrorCode::InvalidParams);
    assert_eq!(c.err("read_resource", json!({"path": "/dev/gtkmcp/fixture/hello.txt", "max_bytes": 100_000_000u64})).await.code, ErrorCode::InvalidParams);
}

// ---- object_stats ----------------------------------------------------------------

async fn gtk_window_count(c: &Client, session: &str) -> u64 {
    let r = c.json("object_stats", json!({"session": session, "type_prefix": "GtkWindow", "limit": 1000})).await;
    r["types"].as_array().unwrap().iter().find(|t| t["type"] == "GtkWindow").map_or(0, |t| t["count"].as_u64().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn object_stats_unavailable_without_instance_count() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, b) = app_broker(&fx);
    let apps = c.json("list_apps", json!({})).await;
    assert!(!apps[0]["hello"]["capabilities"].as_array().unwrap().contains(&json!("instance_count")), "{apps}");
    let e = c.err("object_stats", json!({})).await;
    assert_eq!(e.code, ErrorCode::Unavailable);
    assert!(e.message.contains("GOBJECT_DEBUG=instance-count"), "{}", e.message);
    drop(b);
}

#[tokio::test(flavor = "multi_thread")]
async fn object_stats_counts_grow_with_the_instance_count_preset() {
    let display = require_display!();
    let a = assets();
    let dir = tempfile::tempdir().unwrap();
    let (c, _b) = launcher(dir.path(), true);
    let mut args = launch_json(&display, &a, fixture_argv(&[]));
    args["presets"]["instance_count"] = json!(true);
    let r = c.json("launch_app", args).await;
    assert!(r["hello"]["capabilities"].as_array().unwrap().contains(&json!("instance_count")), "{r}");
    let sid = r["session"].as_str().unwrap();

    // GLib only counts when built with debug support; the agent probes and says so.
    let first = c.call("object_stats", json!({"session": sid, "limit": 5})).await;
    if let Err(e) = first {
        assert_eq!(e.code, ErrorCode::Unavailable);
        assert!(e.message.contains("does not count instances"), "{}", e.message);
        eprintln!("object_stats growth check skipped: {}", e.message);
        return;
    }

    let stats = c.json("object_stats", json!({"session": sid, "limit": 5})).await;
    assert_eq!(stats["types"].as_array().unwrap().len(), 5);
    assert!(stats["total_instances"].as_i64().unwrap() > 100, "{stats}");
    let counts: Vec<i64> = stats["types"].as_array().unwrap().iter().map(|t| t["count"].as_i64().unwrap()).collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "sorted by count: {counts:?}");
    assert!(stats["truncated"].as_bool().unwrap());

    let before = gtk_window_count(&c, sid).await;
    for _ in 0..2 {
        c.json("interact", json!({"session": sid, "target": by_id("open-second-window"), "operation": "activate"})).await;
    }
    until("two more GtkWindow instances", Duration::from_secs(5), async || (gtk_window_count(&c, sid).await == before + 2).then_some(())).await;
    let r = c.json("object_stats", json!({"session": sid, "type_prefix": "GtkWindow", "min_count": 2})).await;
    assert!(r["types"].as_array().unwrap().iter().all(|t| t["count"].as_i64().unwrap() >= 2));
}

// ---- register_state resources ----------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn register_state_is_an_mcp_resource_and_reregistration_replaces_it() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let (c, b) = app_broker(&fx);
    let list = b.list_state_resources().await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].name, "fixture");
    assert_eq!(list[0].uri, "gtk-mcp://s1/app-state/fixture");
    let v = b.read_state_resource(&list[0].uri).await.unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["activations"], 1);
    assert_eq!(v["opened"], json!([]));
    // the closure runs on the app's main thread each time: live state
    c.json("app_open", json!({"files": ["/tmp/m4-state.txt"]})).await;
    let v = b.read_state_resource(&list[0].uri).await.unwrap();
    assert_eq!(v["opened"], json!(["file:///tmp/m4-state.txt"]));

    c.json("interact", json!({"target": by_id("reregister-state"), "operation": "activate"})).await;
    let v = b.read_state_resource(&list[0].uri).await.unwrap();
    assert_eq!(v, json!({"version": 2, "replaced": true}));
    assert_eq!(b.list_state_resources().await.len(), 1, "re-registration replaces, never duplicates");

    assert_eq!(b.read_state_resource("gtk-mcp://s1/app-state/nope").await.unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(b.read_state_resource("gtk-mcp://s9/app-state/fixture").await.unwrap_err().code, ErrorCode::InvalidParams);
    assert_eq!(b.read_state_resource("file:///etc/passwd").await.unwrap_err().code, ErrorCode::InvalidParams);
    assert_eq!(b.read_state_resource("gtk-mcp://s1/other/x").await.unwrap_err().code, ErrorCode::InvalidParams);

    // state resources belong to the `app` toolset: absent with the default policy
    let core_only = fx.broker();
    assert!(core_only.list_state_resources().await.is_empty());
    assert_eq!(core_only.read_state_resource("gtk-mcp://s1/app-state/fixture").await.unwrap_err().code, ErrorCode::NotFound);
}

struct Stdio3 {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next: u64,
}

impl Stdio3 {
    fn start(dir: &Path, args: &[&str]) -> Stdio3 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gtk-mcp"))
            .args(args)
            .env("GTK_MCP_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v);
                }
            }
        });
        let mut c = Stdio3 { child, stdin, rx, next: 1 };
        let init = c.call("initialize", json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}));
        assert!(init["result"]["capabilities"].get("resources").is_some() == args.contains(&"core,app"), "{init}");
        writeln!(c.stdin, "{}", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap();
        c
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let v = self.rx.recv_timeout(Duration::from_secs(20)).expect("response within 20 s");
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }
}

impl Drop for Stdio3 {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn register_state_over_mcp_stdio_resources_list_and_read() {
    let display = require_display!();
    let fx = Fixture::start(&display, &[]);
    let dir = fx.dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut c = Stdio3::start(&dir, &["--toolsets", "core,app"]);
        let r = c.call("resources/list", json!({}));
        let res = r["result"]["resources"].as_array().unwrap_or_else(|| panic!("{r}"));
        assert_eq!(res.len(), 1, "{r}");
        assert_eq!(res[0]["uri"], "gtk-mcp://s1/app-state/fixture");
        assert_eq!(res[0]["mimeType"], "application/json");
        let r = c.call("resources/read", json!({"uri": "gtk-mcp://s1/app-state/fixture"}));
        let text = r["result"]["contents"][0]["text"].as_str().unwrap_or_else(|| panic!("{r}"));
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["version"], 1);
        let r = c.call("resources/read", json!({"uri": "gtk-mcp://s1/app-state/missing"}));
        assert!(r.get("error").is_some(), "{r}");
        // the app tools are listed with the toolset
        let tools = c.call("tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        for t in ["launch_app", "restart_app", "stop_app", "app_open", "inspect_gsettings", "set_gsetting", "list_resources", "read_resource"] {
            assert!(names.contains(&t), "{t} missing from {names:?}");
        }
        assert!(!names.contains(&"object_stats"), "object_stats is in the diagnostics toolset");
        drop(c);
        // default toolsets: no resources capability, no app tools
        let mut c = Stdio3::start(&dir, &[]);
        let tools = c.call("tools/list", json!({}));
        assert!(!tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "set_gsetting"));
        let r = c.call("resources/list", json!({}));
        assert!(r.get("error").is_some() || r["result"]["resources"].as_array().is_none_or(Vec::is_empty), "{r}");
    })
    .await
    .unwrap();
}
