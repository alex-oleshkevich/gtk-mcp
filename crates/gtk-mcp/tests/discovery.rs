mod common;

use common::*;
use gtk_mcp::Policy;
use gtk_mcp_protocol::*;
use serde_json::{Value, json};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::time::{Duration, Instant};

async fn list(b: &std::sync::Arc<gtk_mcp::Broker>) -> Vec<Value> {
    let out = b.call_tool("list_apps", json!({})).await.unwrap();
    out.json_content().unwrap().as_array().unwrap().clone()
}

#[tokio::test]
async fn discovery_running_mock_is_listed_with_hello() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::builder(dir.path()).start();
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    let apps = list(&b).await;
    assert_eq!(apps.len(), 1);
    let info: SessionInfo = serde_json::from_value(apps[0].clone()).unwrap();
    assert_eq!(info.session, "s1");
    assert_eq!(info.pid, mock.pid);
    assert_eq!(info.state, SessionState::Running);
    assert!(!info.launched_by_broker);
    assert_eq!(info.app_id.as_deref(), Some("org.example.Mock"));
    let hello = info.hello.unwrap();
    assert_eq!(hello.gtk_version, [4, 22, 1]);
    assert_eq!(hello.adw_version, Some([1, 9, 0]));
    assert_eq!(hello.capabilities, vec![Capability::Adwaita, Capability::AccessibleId]);
    // ids are stable across rescans
    assert_eq!(list(&b).await[0]["session"], "s1");
}

#[tokio::test]
async fn discovery_stale_socket_of_dead_pid_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let path = dir.path().join(format!("{dead}.sock"));
    drop(UnixListener::bind(&path).unwrap()); // leaves the socket file behind
    let far = dir.path().join("4000000.sock");
    drop(UnixListener::bind(&far).unwrap());
    let other = dir.path().join("notes.txt");
    std::fs::write(&other, b"x").unwrap();

    let live = MockAgent::start(dir.path());
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    let apps = list(&b).await;
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["pid"], live.pid);
    assert!(!path.exists(), "dead pid socket must be unlinked");
    assert!(!far.exists());
    assert!(other.exists(), "unrelated files are left alone");
    assert!(live.path.exists());
}

#[tokio::test]
async fn discovery_refused_socket_of_live_pid_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    // Our own pid is alive, but nothing listens on the socket: ECONNREFUSED.
    let path = dir.path().join(format!("{}.sock", std::process::id()));
    drop(UnixListener::bind(&path).unwrap());
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    assert!(list(&b).await.is_empty());
    assert!(!path.exists());
}

#[tokio::test]
async fn discovery_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let b = broker_with(&dir.path().join("nope"), Policy::default(), |_| {});
    assert!(list(&b).await.is_empty());
}

#[tokio::test]
async fn discovery_hung_handshake_is_skipped_not_unlinked() {
    let dir = tempfile::tempdir().unwrap();
    let hung = MockAgent::builder(dir.path()).hang_on_hello().start();
    let b = broker_with(dir.path(), Policy::default(), |c| c.handshake_timeout = Duration::from_millis(300));
    let t = Instant::now();
    assert!(list(&b).await.is_empty());
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(hung.path.exists());
}

#[tokio::test]
async fn discovery_protocol_mismatch_is_incompatible() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::builder(dir.path()).ack(|a| a.protocol = 99).start();
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    let apps = list(&b).await;
    assert_eq!(apps[0]["state"], "incompatible");
    assert_eq!(apps[0]["hello"]["protocol"], 99);
    for tool in ["list_windows", "interact"] {
        let args = if tool == "interact" { json!({"session":"s1","target":"w1","operation":"activate"}) } else { json!({"session":"s1"}) };
        let e = b.call_tool(tool, args).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::VersionMismatch, "{tool}");
    }
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn discovery_eof_marks_exited_with_in_flight_and_keeps_events() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::builder(dir.path())
        .script("list_windows", vec![Step::Started, Step::Event(json!({"kind":"notify","n":1})), Step::Sleep(100), Step::Close])
        .start();
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    list(&b).await;
    mock.push("$/log", json!({"level":"warning","message":"boom"}));
    let e = b.call_tool("list_windows", json!({"session":"s1"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SessionExited);
    let (id, ..) = mock.requests()[0].clone();
    assert_eq!(e.data.as_ref().unwrap()["in_flight"], id);

    let s = b.session("s1").unwrap();
    assert_eq!(s.state(), SessionState::Exited { code: None, signal: None, in_flight: Some(id) });
    // still listed, events and logs retained
    let apps = list(&b).await;
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["state"]["exited"]["in_flight"], id);
    assert_eq!(s.events_since(0).entries[0].value["kind"], "notify");
    assert_eq!(s.logs_since(0).entries[0].value["message"], "boom");
    // further calls fail fast
    let e = b.call_tool("list_windows", json!({"session":"s1"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SessionExited);
}

#[tokio::test]
async fn discovery_exited_sessions_are_bounded_by_retain_exited() {
    let dir = tempfile::tempdir().unwrap();
    let a = MockAgent::start(dir.path());
    let b_mock = MockAgent::start(dir.path());
    let b = broker_with(dir.path(), Policy::default(), |c| c.retain_exited = 1);
    assert_eq!(list(&b).await.len(), 2);
    a.close_connections();
    assert!(wait_until(|| matches!(b.session("s1").unwrap().state(), SessionState::Exited { .. })));
    b_mock.close_connections();
    assert!(wait_until(|| b.sessions().iter().all(|s| matches!(s.state(), SessionState::Exited { .. }))));
    let kept: Vec<String> = b.sessions().iter().map(|s| s.id().to_owned()).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_ne!(kept[0], "s1", "the oldest exited session is dropped first");
}

#[tokio::test]
async fn discovery_session_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let _a = MockAgent::start(dir.path());
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    // a miss rescans: no list_apps call needed, the only running session is used
    let out = b.call_tool("list_windows", json!({})).await.unwrap();
    assert_eq!(out.json_content().unwrap()["echo"], "list_windows");

    let _b2 = MockAgent::start(dir.path());
    // an unambiguous empty session does not rescan, so s2 is still unknown here
    let out = b.call_tool("list_windows", json!({})).await.unwrap();
    assert_eq!(out.json_content().unwrap()["echo"], "list_windows");
    assert_eq!(list(&b).await.len(), 2);
    let e = b.call_tool("list_windows", json!({})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidParams, "ambiguous without a session");
    let out = b.call_tool("list_windows", json!({"session":"s2"})).await.unwrap();
    assert_eq!(out.json_content().unwrap()["echo"], "list_windows");
    let e = b.call_tool("list_windows", json!({"session":"s9"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(e.message.contains("s1") && e.message.contains("s2"), "{}", e.message);
}

/// gm-dl5: the socket's pid (its name) must be the pid of the process that
/// serves it and the pid its hello_ack reports; anything else is skipped and
/// left alone (it is not stale), since the pid drives stop signals.
#[tokio::test]
async fn discovery_requires_peer_pid_and_hello_pid_to_match_the_socket_name() {
    use std::io::BufReader;
    let dir = tempfile::tempdir().unwrap();
    // Served by this test process, named after a live `sleep`.
    let mut other = Command::new("sleep").arg("60").spawn().unwrap();
    let impostor = dir.path().join(format!("{}.sock", other.id()));
    let listener = UnixListener::bind(&impostor).unwrap();
    let ack = default_ack(other.id());
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let ack = ack.clone();
            std::thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                let mut r = FrameReader::new(BufReader::new(conn));
                while let Ok(Some(Frame::Line(line))) = r.next_frame() {
                    if let Ok(Message::Request(req)) = parse_message(&line) {
                        let _ = write_frame(&mut w, &Response::ok(req.id, serde_json::to_value(&ack).unwrap()));
                    }
                }
            });
        }
    });
    // Served by its own process, but the hello_ack names another pid.
    let lying = MockAgent::builder(dir.path()).ack(|a| a.pid += 1).start();
    let honest = MockAgent::start(dir.path());

    let b = broker_with(dir.path(), Policy::default(), |_| {});
    let apps = list(&b).await;
    assert_eq!(apps.len(), 1, "{apps:?}");
    assert_eq!(apps[0]["pid"], honest.pid);
    assert!(impostor.exists() && lying.path.exists(), "live sockets are skipped, not unlinked");
    other.kill().unwrap();
    other.wait().unwrap();
}

/// gm-5y5: an exited session still blocks the very process it belonged to
/// (its socket is still there), so the app is not adopted a second time.
#[tokio::test]
async fn discovery_exited_session_still_blocks_its_own_process() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockAgent::start(dir.path());
    let b = broker_with(dir.path(), Policy::default(), |_| {});
    assert_eq!(list(&b).await.len(), 1);
    mock.close_connections();
    assert!(wait_until(|| matches!(b.session("s1").unwrap().state(), SessionState::Exited { .. })));
    let apps = list(&b).await;
    assert_eq!(apps.len(), 1, "{apps:?}");
    assert_eq!(apps[0]["session"], "s1");
}
