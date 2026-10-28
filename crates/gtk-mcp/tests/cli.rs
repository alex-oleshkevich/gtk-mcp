mod common;

use common::MockAgent;
use gtk_mcp_protocol::TOOLS;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_gtk-mcp");

fn startup_failure(args: &[&str], needle: &str) {
    let out = Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap();
    assert!(!out.status.success(), "{args:?} must fail startup");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(needle), "stderr for {args:?}: {err}");
    assert!(out.stdout.is_empty(), "stdout is MCP only");
}

#[test]
fn cli_unknown_tool_in_tools_fails_startup() {
    startup_failure(&["--tools", "nope"], "nope");
    startup_failure(&["--tools", "snapshot,nope"], "nope");
}

#[test]
fn cli_unknown_tool_in_exclude_tools_fails_startup() {
    startup_failure(&["--exclude-tools", "nope"], "nope");
}

#[test]
fn cli_toolsets_flag_is_rejected() {
    startup_failure(&["--toolsets", "core"], "--toolsets");
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next: u64,
}

impl Client {
    fn start(dir: &std::path::Path, args: &[&str]) -> Client {
        let mut child = Command::new(BIN)
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
        let mut c = Client { child, stdin, rx, next: 1 };
        let init = c.call(
            "initialize",
            json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
        );
        assert!(init.get("result").is_some(), "{init}");
        c.notify("notifications/initialized");
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(json!({"jsonrpc":"2.0","method":method}));
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let v = self.rx.recv_timeout(Duration::from_secs(20)).expect("response");
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }

    fn tools(&mut self) -> Vec<Value> {
        self.call("tools/list", json!({}))["result"]["tools"].as_array().unwrap().clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_lists_all_56_tools_by_default_with_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path(), &[]);
    let tools = c.tools();
    assert_eq!(tools.len(), 56);
    let names: BTreeSet<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS.iter().map(|t| t.name).collect());
    for t in &tools {
        let spec = TOOLS.iter().find(|s| s.name == t["name"].as_str().unwrap()).unwrap();
        let a = &t["annotations"];
        if spec.mutating {
            assert_eq!(a["destructiveHint"], true, "{}", t["name"]);
            assert_ne!(a["readOnlyHint"], true, "{}", t["name"]);
        } else {
            assert_eq!(a["readOnlyHint"], true, "{}", t["name"]);
        }
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
    }
    assert!(tools.iter().any(|t| t["name"] == "batch" && t["annotations"]["readOnlyHint"] != true));
}

#[test]
fn stdio_exclude_tools_removes_from_list_and_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path(), &["--exclude-tools", "capture"]);
    let tools = c.tools();
    assert_eq!(tools.len(), TOOLS.len() - 1);
    assert!(!tools.iter().any(|t| t["name"] == "capture"));
    let r = c.call("tools/call", json!({"name":"capture","arguments":{"target":"w1"}}));
    assert!(r.get("error").is_some() || r["result"]["isError"] == true, "{r}");
}

#[test]
fn stdio_tools_flag_restricts() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path(), &["--tools", "snapshot,list_apps"]);
    let names: BTreeSet<String> = c.tools().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, BTreeSet::from(["snapshot".to_owned(), "list_apps".to_owned()]));
}

#[test]
fn stdio_list_apps_uses_gtk_mcp_dir_and_returns_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path(), &[]);
    let r = c.call("tools/call", json!({"name":"list_apps","arguments":{}}));
    assert_eq!(r["result"]["content"][0]["text"], "[]");

    let mock = MockAgent::start(dir.path());
    let r = c.call("tools/call", json!({"name":"list_apps","arguments":{}}));
    let apps: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(apps[0]["pid"], mock.pid);
    assert_eq!(apps[0]["state"], "running");
}

#[test]
fn stdio_mutating_tool_forwards_without_any_flag() {
    let dir = tempfile::tempdir().unwrap();
    let _mock = MockAgent::start(dir.path());
    let mut c = Client::start(dir.path(), &[]);
    // no mutation gate: the call is forwarded to the agent (mock echoes it)
    let r = c.call("tools/call", json!({"name":"interact","arguments":{"target":"w1","operation":"activate"}}));
    assert_ne!(r["result"]["isError"], true, "{r}");
    let v: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["echo"], "interact");
}
