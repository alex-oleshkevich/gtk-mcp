//! `gtk-mcp apps|call|tools|doctor` against mock agents (no display needed).

mod common;

use base64::Engine;
use common::MockAgent;
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_gtk-mcp");

/// A temp dir with the 0700 mode agents create their socket directory with.
fn private_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) -> Output {
    run_stdin(dir, args, "")
}

fn run_stdin(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .env("GTK_MCP_DIR", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn apps_lists_the_mock() {
    let dir = private_dir();
    let mock = MockAgent::start(dir.path());
    let o = run(dir.path(), &["apps"]);
    assert!(o.status.success(), "{}", err(&o));
    let table = out(&o);
    let header = table.lines().next().unwrap();
    for col in ["SESSION", "PID", "APP ID", "GTK", "STATE"] {
        assert!(header.contains(col), "{table}");
    }
    let row = table.lines().nth(1).unwrap();
    assert!(row.contains("s1") && row.contains(&mock.pid.to_string()), "{table}");
    assert!(row.contains("org.example.Mock") && row.contains("4.22.1") && row.contains("running"), "{table}");

    let o = run(dir.path(), &["apps", "--json"]);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v[0]["pid"], mock.pid);
    assert_eq!(v[0]["session"], "s1");
    assert_eq!(v[0]["state"], "running");
}

#[test]
fn apps_with_none_running_says_so() {
    let dir = private_dir();
    let o = run(dir.path(), &["apps"]);
    assert!(o.status.success());
    assert!(out(&o).contains("no instrumented apps"));
    assert_eq!(serde_json::from_str::<Value>(&out(&run(dir.path(), &["apps", "--json"]))).unwrap(), json!([]));
}

#[test]
fn call_runs_a_tool_on_the_only_app_and_prints_pretty_json() {
    let dir = private_dir();
    let mock = MockAgent::start(dir.path());
    let o = run(dir.path(), &["call", "interact", r#"{"target":"w1","operation":"activate"}"#]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("\n  \"echo\""), "pretty printed: {text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["echo"], "interact");
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].2["target"], "w1");
}

#[test]
fn call_reads_args_from_stdin() {
    let dir = private_dir();
    let mock = MockAgent::start(dir.path());
    let o = run_stdin(dir.path(), &["call", "interact", "-"], r#"{"target":"w7","operation":"activate"}"#);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(mock.requests()[0].2["target"], "w7");
}

#[test]
fn call_app_selects_by_session_pid_and_app_id() {
    let dir = private_dir();
    let a = MockAgent::builder(dir.path()).ack(|a| a.app_id = Some("org.example.A".into())).start();
    let b = MockAgent::builder(dir.path()).ack(|a| a.app_id = Some("org.example.B".into())).start();
    let args = r#"{"target":"w1","operation":"activate"}"#;
    let (first, second) = if a.pid < b.pid { (&a, &b) } else { (&b, &a) };

    let o = run(dir.path(), &["call", "interact", args, "--app", &second.pid.to_string()]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(second.requests().len(), 1);
    assert!(first.requests().is_empty());

    // Sessions are numbered in pid order.
    let o = run(dir.path(), &["call", "interact", args, "--app", "s1"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(first.requests().len(), 1);

    let before = b.requests().len();
    let o = run(dir.path(), &["call", "interact", args, "--app", "org.example.B"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(b.requests().len(), before + 1);

    // An explicit session in ARGS_JSON wins over --app.
    let before = second.requests().len();
    let o = run(dir.path(), &["call", "interact", r#"{"session":"s2","target":"w1","operation":"activate"}"#, "--app", "s1"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(second.requests().len(), before + 1);

    let o = run(dir.path(), &["call", "interact", args, "--app", "nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no app matches 'nope'"), "{}", err(&o));
}

#[test]
fn call_with_two_apps_and_no_app_lists_them() {
    let dir = private_dir();
    let a = MockAgent::start(dir.path());
    let b = MockAgent::start(dir.path());
    let o = run(dir.path(), &["call", "interact", r#"{"target":"w1","operation":"activate"}"#]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).is_empty(), "stdout stays empty on errors: {}", out(&o));
    let e: Value = serde_json::from_str(&err(&o)).expect("AgentError JSON on stderr");
    assert_eq!(e["code"], "invalid_params");
    let msg = e["message"].as_str().unwrap();
    assert!(msg.contains(&a.pid.to_string()) && msg.contains(&b.pid.to_string()), "{msg}");
}

#[test]
fn call_unknown_tool_suggests_close_names() {
    let dir = private_dir();
    let o = run(dir.path(), &["call", "snapshoot"]);
    assert_eq!(o.status.code(), Some(1));
    let e: Value = serde_json::from_str(&err(&o)).unwrap();
    let msg = e["message"].as_str().unwrap();
    assert!(msg.contains("unknown tool 'snapshoot'") && msg.contains("snapshot"), "{msg}");
}

#[test]
fn call_rejects_bad_json_and_non_objects() {
    let dir = private_dir();
    let o = run(dir.path(), &["call", "list_apps", "{nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("not valid JSON"), "{}", err(&o));
    let o = run(dir.path(), &["call", "list_apps", "[1]"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("must be a JSON object"), "{}", err(&o));
}

#[test]
fn call_list_apps_needs_no_app() {
    let dir = private_dir();
    let _a = MockAgent::start(dir.path());
    let _b = MockAgent::start(dir.path());
    let o = run(dir.path(), &["call", "list_apps"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap().as_array().unwrap().len(), 2);
}

#[test]
fn call_capture_o_writes_the_png_and_prints_no_base64() {
    let dir = private_dir();
    let png = common::png(4, 3, |x, y| [x as u8 * 10, y as u8 * 10, 0, 255]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let _mock = MockAgent::builder(dir.path())
        .respond("capture", json!({"capture_id": "c1", "width": 4, "height": 3, "png_base64": b64}))
        .start();
    let file = dir.path().join("shot.png");
    let o = run(dir.path(), &["call", "capture", r#"{"target":"w1"}"#, "-o", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(std::fs::read(&file).unwrap(), png);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["capture_id"], "c1");
    assert!(!out(&o).contains("png_base64") && !out(&o).contains(&b64));
    assert!(err(&o).contains("wrote"), "{}", err(&o));

    // Without -o the image is dropped from the output and a note says so.
    let o = run(dir.path(), &["call", "capture", r#"{"target":"w1"}"#]);
    assert!(o.status.success());
    assert!(!out(&o).contains("png_base64"));
    assert!(err(&o).contains("-o FILE"), "{}", err(&o));
}

#[test]
fn tools_lists_all_56_with_17_mutating() {
    let dir = private_dir();
    let o = run(dir.path(), &["tools", "--json"]);
    assert!(o.status.success());
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    let sets: Vec<&str> = v.as_array().unwrap().iter().map(|s| s["toolset"].as_str().unwrap()).collect();
    assert_eq!(sets, ["core", "actions", "layout-style", "a11y", "diagnostics", "app"]);
    let tools: Vec<&Value> = v.as_array().unwrap().iter().flat_map(|s| s["tools"].as_array().unwrap()).collect();
    assert_eq!(tools.len(), 56);
    assert_eq!(tools.iter().filter(|t| t["mutating"] == true).count(), 17);

    let o = run(dir.path(), &["tools"]);
    let text = out(&o);
    assert_eq!(text.lines().filter(|l| l.starts_with("  M ")).count(), 17, "{text}");
    assert_eq!(text.lines().filter(|l| l.starts_with("  R ")).count(), 39, "{text}");
    assert!(text.contains("core (") && text.contains("  R list_apps") && text.contains("  M interact"), "{text}");
}

#[test]
fn serve_subcommand_and_no_subcommand_both_start_the_server() {
    for args in [vec!["serve", "--tools", "nope"], vec!["--tools", "nope"]] {
        let dir = private_dir();
        let o = run(dir.path(), &args);
        assert!(!o.status.success());
        assert!(err(&o).contains("nope"), "{args:?}: {}", err(&o));
    }
}

fn doctor_json(dir: &Path) -> (Option<i32>, Value) {
    let o = run(dir, &["doctor", "--json"]);
    let v = serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{e}: {} / {}", out(&o), err(&o)));
    (o.status.code(), v)
}

#[test]
fn doctor_reports_a_healthy_mock() {
    let dir = private_dir();
    let mock = MockAgent::start(dir.path());
    let (code, v) = doctor_json(dir.path());
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["healthy"], true);
    assert_eq!(v["socket_dir"]["source"], "GTK_MCP_DIR");
    assert_eq!(v["socket_dir"]["mode"], "0700");
    assert_eq!(v["socket_dir"]["owned_by_us"], true);
    let s = &v["sockets"][0];
    assert_eq!(s["pid"], mock.pid);
    assert_eq!(s["alive"], true);
    assert_eq!(s["status"], "ok");
    assert_eq!(s["hello"]["app_id"], "org.example.Mock");
    assert_eq!(s["hello"]["gtk_version"], json!([4, 22, 1]));
    assert!(v["atspi"]["reachable"].is_boolean());

    let o = run(dir.path(), &["doctor"]);
    assert_eq!(o.status.code(), Some(0));
    let text = out(&o);
    for needle in ["GTK_MCP_DIR", "mode: 0700", "handshake: ok", "agent version: 0.0.0-mock", "org.example.Mock", "GTK: 4.22.1", "libadwaita: 1.9.0", "adwaita, accessible_id", "AT-SPI bus:", "healthy"] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }
}

#[test]
fn doctor_with_no_apps_or_no_dir_is_healthy() {
    let dir = private_dir();
    let (code, v) = doctor_json(dir.path());
    assert_eq!(code, Some(0));
    assert_eq!(v["sockets"], json!([]));
    let missing = dir.path().join("not-created");
    let (code, v) = doctor_json(&missing);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["socket_dir"]["exists"], false);
}

#[test]
fn doctor_reports_a_stale_socket_and_leaves_it() {
    let dir = private_dir();
    let mut dead = Command::new("true").spawn().unwrap();
    dead.wait().unwrap();
    let path = dir.path().join(format!("{}.sock", dead.id()));
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());

    let (code, v) = doctor_json(dir.path());
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["sockets"][0]["status"], "stale");
    assert_eq!(v["sockets"][0]["alive"], false);
    assert!(path.exists(), "doctor must not unlink stale sockets");
    let text = out(&run(dir.path(), &["doctor"]));
    assert!(text.contains("STALE") && text.contains("dead"), "{text}");
    assert!(path.exists());
}

#[test]
fn doctor_exits_1_when_the_dir_is_unusable() {
    let tmp = private_dir();
    let file = tmp.path().join("afile");
    std::fs::write(&file, "x").unwrap();
    let (code, v) = doctor_json(&file);
    assert_eq!(code, Some(1));
    assert_eq!(v["healthy"], false);
    assert!(v["socket_dir"]["problem"].as_str().unwrap().contains("not a directory"));

    let open = tmp.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let o = run(&open, &["doctor"]);
    assert_eq!(o.status.code(), Some(1));
    let text = out(&o);
    assert!(text.contains("PROBLEM") && text.contains("expected 0700") && text.contains("UNHEALTHY"), "{text}");
}

#[test]
fn doctor_exits_1_when_a_handshake_fails() {
    let dir = private_dir();
    let _mock = MockAgent::builder(dir.path()).hang_on_hello().start();
    let (code, v) = doctor_json(dir.path());
    assert_eq!(code, Some(1), "{v}");
    assert_eq!(v["sockets"][0]["status"], "error");
    assert!(v["sockets"][0]["error"].as_str().unwrap().contains("no hello_ack"));
}

#[test]
fn doctor_exits_1_on_a_protocol_mismatch() {
    let dir = private_dir();
    let _mock = MockAgent::builder(dir.path()).ack(|a| a.protocol = 99).start();
    let (code, v) = doctor_json(dir.path());
    assert_eq!(code, Some(1), "{v}");
    assert_eq!(v["sockets"][0]["status"], "protocol_mismatch");
    assert_eq!(v["sockets"][0]["hello"]["protocol"], 99);
}
