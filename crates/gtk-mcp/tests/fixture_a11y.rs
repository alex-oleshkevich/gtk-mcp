//! M6 acceptance (AC-13) and spike S3 measurements against the real fixture,
//! on a private session bus with its own a11y bus: nothing here touches the
//! user's accessibility bus. Tests are named `a11y_*`.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, BusConfig, Config, Policy};
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// A private D-Bus session bus whose `org.a11y.Bus` is activated from the
/// system's `at-spi-bus-launcher`, with a private runtime dir so its a11y bus
/// socket cannot collide with the user's.
struct PrivateBus {
    address: String,
    runtime: std::path::PathBuf,
}

/// Directory removed when the test process exits (statics are never dropped).
static BUS_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

extern "C" fn remove_bus_dir() {
    if let Some(d) = BUS_DIR.get() {
        let _ = std::fs::remove_dir_all(d);
    }
}

static BUS: LazyLock<Option<PrivateBus>> = LazyLock::new(start_bus);

fn start_bus() -> Option<PrivateBus> {
    let dir = tempfile::tempdir().ok()?.keep();
    let _ = BUS_DIR.set(dir.clone());
    // SAFETY: registers a plain function that only removes a directory.
    unsafe { libc::atexit(remove_bus_dir) };
    let root = dir.as_path();
    let runtime = root.join("runtime");
    let services = root.join("services");
    std::fs::create_dir_all(&runtime).ok()?;
    std::fs::create_dir_all(&services).ok()?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    let launcher = ["/usr/lib/at-spi-bus-launcher", "/usr/libexec/at-spi-bus-launcher", "/usr/lib/at-spi2-core/at-spi-bus-launcher"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists());
    let Some(launcher) = launcher else {
        eprintln!("a11y tests SKIPPED: at-spi-bus-launcher not found");
        return None;
    };
    std::fs::write(
        services.join("org.a11y.Bus.service"),
        format!("[D-BUS Service]\nName=org.a11y.Bus\nExec={launcher} --launch-immediately\n"),
    )
    .ok()?;
    let sock = runtime.join("bus");
    let conf = root.join("bus.conf");
    std::fs::write(
        &conf,
        format!(
            "<busconfig><type>session</type><listen>unix:path={}</listen><servicedir>{}</servicedir>\
             <policy context='default'><allow send_destination='*' eavesdrop='true'/><allow eavesdrop='true'/><allow own='*'/></policy></busconfig>",
            sock.display(),
            services.display()
        ),
    )
    .ok()?;
    let mut cmd = Command::new("dbus-daemon");
    cmd.args(["--config-file"])
        .arg(&conf)
        .args(["--nofork", "--print-address=1"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("AT_SPI_BUS_ADDRESS")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe prctl between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    // PDEATHSIG tracks the forking thread: keep it alive for the whole test process.
    let registryd = ["/usr/lib/at-spi2-registryd", "/usr/libexec/at-spi2-registryd", "/usr/lib/at-spi2/at-spi2-registryd", "/usr/lib/at-spi2-core/at-spi2-registryd"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())?;
    let runtime2 = runtime.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    std::thread::Builder::new()
        .name("dbus-parent".into())
        .spawn(move || {
            let mut child: Child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("a11y tests SKIPPED: cannot start dbus-daemon: {e}");
                    let _ = tx.send(None);
                    return;
                }
            };
            use std::io::BufRead;
            let mut line = String::new();
            let _ = std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
            let address = line.trim().to_string();
            // Finding (S3): a private session bus activates org.a11y.Bus (the a11y bus) but never
            // the registry daemon (no systemd), so the harness owns one, like a desktop session does.
            let mut reg = Command::new(registryd);
            reg.env("DBUS_SESSION_BUS_ADDRESS", &address)
                .env("XDG_RUNTIME_DIR", &runtime2)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: only async-signal-safe prctl between fork and exec.
            unsafe {
                reg.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
            let mut reg_child = reg.spawn().ok();
            std::thread::sleep(Duration::from_millis(800));
            let _ = tx.send(Some(address));
            let _ = child.wait();
            if let Some(mut r) = reg_child.take() {
                let _ = r.kill();
            }
        })
        .ok()?;
    let address = rx.recv_timeout(Duration::from_secs(10)).ok()??;
    if address.is_empty() {
        return None;
    }
    Some(PrivateBus { address, runtime })
}

macro_rules! require_bus {
    () => {
        match BUS.as_ref() {
            Some(b) => b,
            None => return,
        }
    };
}

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

fn start_fixture(display: &Display, bus: &PrivateBus, mutations: bool) -> Fixture {
    Fixture::start(
        display,
        mutations,
        &[
            ("GTK_A11Y", "atspi"),
            ("DBUS_SESSION_BUS_ADDRESS", bus.address.as_str()),
            ("GDK_DEBUG", "no-portals"),
            ("GTK_USE_PORTAL", "0"),
        ],
    )
}

fn broker(fx: &Fixture, bus: &PrivateBus, atspi: bool, mutations: bool) -> Client {
    let policy = Policy {
        allow_mutations: mutations,
        allow_atspi: atspi,
        toolsets: vec!["core".into(), "a11y".into()],
        ..Policy::default()
    };
    let mut config = Config::with_socket_dir(fx.dir.path());
    config.atspi_bus = BusConfig { session_bus_address: Some(bus.address.clone()), a11y_bus_address: None };
    let _ = &bus.runtime;
    Client { broker: Broker::new(policy, config).unwrap() }
}

fn find<'a>(n: &'a Value, pred: &dyn Fn(&Value) -> bool) -> Option<&'a Value> {
    find_node(n, pred)
}

fn by_auto<'a>(tree: &'a Value, id: &str) -> &'a Value {
    find(tree, &|n| n["automation_id"] == id).unwrap_or_else(|| panic!("no accessible node {id}"))
}

/// Wait until the app is exported on the a11y bus, i.e. the joined tree reports AT-SPI available.
async fn wait_exported(c: &Client) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let t = c.json("accessible_tree", json!({})).await;
        if t["atspi"]["available"] == true && t["atspi"]["join"]["joined"].as_u64().unwrap_or(0) > 5 {
            return t;
        }
        assert!(Instant::now() < deadline, "fixture never appeared on the a11y bus: {}", t["atspi"]);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ---- spike S3 ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a11y_join_rate_on_fixture() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, false);
    let c = broker(&fx, bus, true, false);
    c.settle_tree().await;
    let t = wait_exported(&c).await;
    let stats = &t["atspi"]["join"];
    eprintln!("S3 join stats: {stats}");
    let rate = stats["join_rate"].as_f64().unwrap();
    assert!(rate >= 0.95, "join rate {rate} too low: {stats}");
    assert!(stats["unjoined_atspi"].as_u64().unwrap() <= 5, "AT-SPI-only nodes: {stats}");
    // Every named control of the a11y section is joined.
    for id in ["a11y-labeled", "a11y-icon-unlabeled", "a11y-icon-labelled", "a11y-entry-unlabeled", "a11y-entry-labelled", "a11y-image", "a11y-tiny", "a11y-tab-1", "a11y-tab-2", "a11y-tab-3"] {
        assert_eq!(by_auto(&t, id)["source"], "gtk+atspi", "{id} not joined");
    }
    // Every joined node: role compatible with the table (no wildcard-only pairings on named controls).
    let labeled = by_auto(&t, "a11y-labeled");
    assert_eq!(labeled["source"], "gtk+atspi", "{labeled}");
    assert_eq!(labeled["atspi"]["name"], "Accessible save");
    assert_eq!(labeled["atspi"]["role"], "button");
}

// ---- AC-13 -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a11y_inspect_accessible_tags_each_field_with_its_source() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, false);
    let c = broker(&fx, bus, true, false);
    c.settle_tree().await;
    wait_exported(&c).await;

    let r = c.json("inspect_accessible", json!({"target": by_id("a11y-labeled")})).await;
    assert_eq!(r["atspi"]["available"], true);
    let f = &r["fields"];
    assert_eq!(f["role"]["source"], "gtk");
    assert_eq!(f["role"]["value"], "button");
    assert_eq!(f["platform_state"]["source"], "gtk");
    assert_eq!(f["bounds"]["source"], "gtk");
    assert_eq!(f["name"]["source"], "atspi");
    assert_eq!(f["name"]["value"], "Accessible save");
    assert_eq!(f["states"]["source"], "atspi");
    assert!(f["states"]["value"].as_array().unwrap().iter().any(|s| s == "focusable"), "{}", f["states"]);
    assert!(f["actions"]["value"].as_array().is_some_and(|a| !a.is_empty()), "button has an action: {}", f["actions"]);
    assert_eq!(r["join"], "structure");

    // The accessible label set through the GTK API surfaces as the AT-SPI name.
    let r = c.json("inspect_accessible", json!({"target": by_id("a11y-icon-labelled")})).await;
    assert_eq!(r["fields"]["name"]["value"], "Add item");
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_source_off_reports_atspi_fields_unavailable_and_rules_skipped() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, false);
    let c = broker(&fx, bus, false, false);
    c.settle_tree().await;

    let r = c.json("inspect_accessible", json!({"target": by_id("a11y-labeled")})).await;
    assert_eq!(r["atspi"]["available"], false);
    assert_eq!(r["fields"]["role"]["source"], "gtk");
    for k in ["name", "description", "states", "relations", "actions"] {
        assert_eq!(r["fields"][k]["available"], false, "{k}: {}", r["fields"][k]);
        assert_eq!(r["fields"][k]["source"], "atspi");
    }

    let tree = c.json("accessible_tree", json!({})).await;
    assert_eq!(tree["atspi"]["available"], false);
    assert!(tree["atspi"]["reason"].as_str().unwrap().contains("--allow-atspi"));
    assert!(by_auto(&tree, "a11y-labeled")["atspi"].is_null());

    let audit = c.json("audit_accessibility", json!({})).await;
    let rules = audit["rules"].as_array().unwrap();
    for rule in rules.iter().filter(|r| r["source"] == "atspi") {
        assert_eq!(rule["status"], "skipped", "{rule}");
        assert!(rule["reason"].as_str().unwrap().contains("--allow-atspi"));
    }
    assert!(rules.iter().filter(|r| r["source"] == "gtk").all(|r| r["status"] == "ran"));
    // GTK rules still find things.
    let findings = audit["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["rule"] == "icon_button_no_label"), "{audit}");
    assert!(!findings.iter().any(|f| f["source"] == "atspi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_audit_finds_unnamed_controls_with_evidence() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, false);
    let c = broker(&fx, bus, true, false);
    c.settle_tree().await;
    let tree = wait_exported(&c).await;
    let unlabeled = by_auto(&tree, "a11y-icon-unlabeled")["ref"].as_str().unwrap().to_string();
    let labelled = by_auto(&tree, "a11y-icon-labelled")["ref"].as_str().unwrap().to_string();
    let plain = by_auto(&tree, "a11y-labeled")["ref"].as_str().unwrap().to_string();
    let tiny = by_auto(&tree, "a11y-tiny")["ref"].as_str().unwrap().to_string();
    let skipped = by_auto(&tree, "a11y-unfocusable")["ref"].as_str().unwrap().to_string();

    let audit = c.json("audit_accessibility", json!({})).await;
    assert!(audit["rules"].as_array().unwrap().iter().all(|r| r["status"] == "ran"), "{}", audit["rules"]);
    let findings = audit["findings"].as_array().unwrap();
    let has = |rule: &str, r: &str| findings.iter().any(|f| f["rule"] == rule && f["ref"] == r);
    assert!(has("missing_name", &unlabeled), "icon-only button must lack an AT-SPI name: {audit}");
    assert!(has("icon_button_no_label", &unlabeled));
    assert!(!has("missing_name", &labelled), "labelled icon button has a name");
    assert!(!has("missing_name", &plain));
    assert!(has("small_target", &tiny));
    assert!(!has("interactive_not_focusable", &plain));
    // Unfocusable controls are reported by the rule only while sensitive and mapped.
    assert!(has("interactive_not_focusable", &skipped), "{audit}");
    let f = findings.iter().find(|f| f["rule"] == "missing_name" && f["ref"] == unlabeled.as_str()).unwrap();
    assert_eq!(f["source"], "atspi");
    assert_eq!(f["evidence"]["icon_name"], "edit-delete-symbolic");

    // Scoped audit and rule filter.
    let scoped = c.json("audit_accessibility", json!({"root": by_id("a11y-section"), "rules": ["missing_name"]})).await;
    assert!(scoped["findings"].as_array().unwrap().iter().all(|f| f["rule"] == "missing_name"));
    assert_eq!(scoped["rules"].as_array().unwrap().len(), 1);
    let e = c.err("audit_accessibility", json!({"rules": ["nope"]})).await;
    assert_eq!(e.code, gtk_mcp_protocol::ErrorCode::InvalidParams);
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_secret_widgets_are_redacted() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, true);
    let c = broker(&fx, bus, true, true);
    c.settle_tree().await;
    wait_exported(&c).await;
    c.json("interact", json!({"target": by_id("password"), "operation": "fill", "text": "hunter2-SECRET-0xC0FFEE"})).await;

    let r = c.json("inspect_accessible", json!({"target": by_id("password")})).await;
    assert_eq!(r["secret"], true);
    let text = r.to_string();
    assert!(!text.contains("hunter2"), "secret leaked: {text}");
    for k in ["name", "text", "value"] {
        assert_eq!(r["fields"][k]["redacted"], true, "{k}: {}", r["fields"][k]);
    }
    let tree = c.json("accessible_tree", json!({})).await;
    assert!(!tree.to_string().contains("hunter2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_accessible_tree_root_and_depth() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, false);
    let c = broker(&fx, bus, true, false);
    c.settle_tree().await;
    wait_exported(&c).await;
    let sub = c.json("accessible_tree", json!({"root": by_id("a11y-tabs")})).await;
    let roots = sub["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["children"].as_array().unwrap().len(), 3, "{}", roots[0]);
    assert!(roots[0]["children"].as_array().unwrap().iter().all(|k| k["source"] == "gtk+atspi"));
    let shallow = c.json("accessible_tree", json!({"root": by_id("a11y-tabs"), "max_depth": 0})).await;
    assert_eq!(shallow["roots"][0]["children"].as_array().unwrap().len(), 0);
    assert_eq!(shallow["roots"][0]["children_truncated"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_trace_focus_order_reports_chain_and_restores_focus() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, true);
    let c = broker(&fx, bus, false, true);
    c.settle_tree().await;
    c.json("interact", json!({"target": by_id("a11y-tab-3"), "operation": "focus"})).await;

    let r = c.json("trace_focus_order", json!({"root": by_id("a11y-tabs")})).await;
    let order: Vec<&str> = r["order"].as_array().unwrap().iter().filter_map(|n| n["automation_id"].as_str()).collect();
    assert_eq!(order, ["a11y-tab-1", "a11y-tab-2", "a11y-tab-3"], "{r}");
    assert_eq!(r["focus_restored"], true);
    assert_eq!(r["loops"], false);
    // Focus is back on the third button.
    let f = c.json("inspect", json!({"target": by_id("a11y-tab-3")})).await;
    assert_eq!(f["flags"]["has_focus"], true, "{f}");

    // Whole window: ends, reaches the section buttons, never the unfocusable one.
    let w = c.json("trace_focus_order", json!({"max_steps": 12})).await;
    assert_eq!(w["end"], "max_steps");
    assert_eq!(w["order"].as_array().unwrap().len(), 12);
    assert_eq!(w["focus_restored"], true);
    assert!(w["unreachable"].is_null(), "unreachable is only judged when the walk completed");

    // The whole section, in Tab order, entries via their inner text.
    let s = c.json("trace_focus_order", json!({"root": by_id("a11y-section")})).await;
    let ids: Vec<&str> = s["order"].as_array().unwrap().iter().filter_map(|n| n["automation_id"].as_str()).collect();
    assert_eq!(
        ids,
        ["a11y-labeled", "a11y-icon-unlabeled", "a11y-icon-labelled", "a11y-tiny", "a11y-tab-1", "a11y-tab-2", "a11y-tab-3"],
        "{s}"
    );
    assert_eq!(s["end"], "end");
    let un: Vec<&str> = s["unreachable"].as_array().unwrap().iter().filter_map(|n| n["automation_id"].as_str()).collect();
    assert!(!un.contains(&"a11y-unfocusable"), "unfocusable widgets are not candidates: {un:?}");

    // Mutating tool: refused without the broker flag.
    let ro = broker(&fx, bus, false, false);
    let e = ro.err("trace_focus_order", json!({})).await;
    assert_eq!(e.code, gtk_mcp_protocol::ErrorCode::MutationsDisabled);
}

#[tokio::test(flavor = "multi_thread")]
async fn a11y_focus_event_carries_the_joined_ref() {
    let display = require_display!();
    let bus = require_bus!();
    let fx = start_fixture(&display, bus, true);
    let c = broker(&fx, bus, true, true);
    c.settle_tree().await;
    let tree = wait_exported(&c).await;
    let target = by_auto(&tree, "a11y-tab-2")["ref"].as_str().unwrap().to_string();

    let session = c.broker.resolve_session("").await.unwrap();
    let start = session.events_since(0).next_cursor;
    c.broker.watch_atspi(&session, Some(vec!["object:state-changed:focused".into()])).await.expect("watch_atspi");

    // Wait for a focus event on the target after moving focus there.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut cursor = start;
    let mut found: Option<Value> = None;
    while found.is_none() && Instant::now() < deadline {
        c.json("interact", json!({"target": by_id("a11y-tab-1"), "operation": "focus"})).await;
        c.json("interact", json!({"target": by_id("a11y-tab-2"), "operation": "focus"})).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let read = session.events_since(cursor);
        cursor = read.next_cursor;
        found = read.entries.into_iter().map(|e| e.value).find(|v| v["kind"] == "atspi" && v["ref"] == target.as_str() && v["detail1"] == 1);
    }
    let ev = found.expect("no AT-SPI focus event mapped to the focused button");
    assert_eq!(ev["source"], "atspi");
    assert_eq!(ev["event"], "object:state-changed:focused");
    assert_eq!(ev["role"], "button");
    assert_eq!(ev["name"], "Second");
    assert!(c.broker.unwatch_atspi(session.id()));
}
