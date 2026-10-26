//! Harness for tests that drive the real `gtk-mcp-fixture` binary through the
//! real broker library.
//!
//! The fixture needs a Wayland display. Order of preference:
//! 1. `GTK_MCP_TEST_WAYLAND_DISPLAY` (+ `GTK_MCP_TEST_XDG_RUNTIME_DIR`) provided by the environment;
//! 2. a private nested `niri` started under the host session (killed with the test process);
//! 3. otherwise the tests print a clear message and skip.

#![allow(dead_code)]

use gtk_mcp::{Broker, Config, Policy, ToolOutcome};
use gtk_mcp_protocol::AgentError;
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Display {
    pub xdg_runtime_dir: PathBuf,
    pub wayland_display: String,
}

static DISPLAY: LazyLock<Option<Display>> = LazyLock::new(detect_display);
static SKIP_NOTE: OnceLock<()> = OnceLock::new();

fn die_with_parent(cmd: &mut Command) {
    // SAFETY: only async-signal-safe prctl between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

/// Host-niri IPC helpers: keep the nested compositor's window off the output the
/// user is working on. Never focuses or raises anything on that output.
mod placement {
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn host(args: &[&str]) -> Option<serde_json::Value> {
        let out = Command::new("niri").arg("msg").arg("--json").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    pub struct Before {
        focused_window: Option<u64>,
        work_output: Option<String>,
    }

    pub fn before() -> Option<Before> {
        std::env::var_os("NIRI_SOCKET")?;
        let focused = host(&["focused-window"]);
        let focused_window = focused.as_ref().and_then(|w| w["id"].as_u64());
        let work_output = host(&["focused-output"]).and_then(|o| o["name"].as_str().map(String::from));
        Some(Before { focused_window, work_output })
    }

    /// Move the nested compositor's window (host app_id `niri`, owned by `pid`)
    /// to a different output than the one in use, then restore the focus.
    pub fn move_away(b: &Before, pid: u32) {
        let target = std::env::var("GTK_MCP_TEST_NESTED_OUTPUT").ok();
        let _ = &b.work_output;
        let Some(target) = target else { return };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let wins = host(&["windows"]).unwrap_or_default();
            if let Some(w) = wins.as_array().into_iter().flatten().find(|w| w["pid"].as_u64() == Some(u64::from(pid))) {
                let id = w["id"].as_u64().unwrap().to_string();
                let _ = Command::new("niri").args(["msg", "action", "move-window-to-monitor", "--id", &id, &target]).status();
                if let Some(prev) = b.focused_window {
                    let _ = Command::new("niri").args(["msg", "action", "focus-window", "--id", &prev.to_string()]).status();
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn start_nested_niri() -> Result<Display, String> {
    let host = std::env::var("WAYLAND_DISPLAY").map_err(|_| "no host WAYLAND_DISPLAY for a nested niri".to_string())?;
    let host_rt = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "no XDG_RUNTIME_DIR".to_string())?;
    let host_socket = if host.starts_with('/') { host } else { format!("{host_rt}/{host}") };
    if Command::new("niri").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(true, |s| !s.success()) {
        return Err("`niri` is not installed".into());
    }
    let root = std::env::temp_dir().join(format!("gtkmcp-niri-{}", std::process::id()));
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let cfg = root.join("config.kdl");
    std::fs::write(&cfg, "output \"winit\" {\n    mode \"1280x800@60\"\n    scale 1\n}\n").map_err(|e| e.to_string())?;
    let log = std::fs::File::create(root.join("niri.log")).map_err(|e| e.to_string())?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    if std::env::var_os("NIRI_SOCKET").is_some() && std::env::var_os("GTK_MCP_TEST_NESTED_OUTPUT").is_none() {
        return Err("host niri detected: set GTK_MCP_TEST_NESTED_OUTPUT=<output name> so the nested compositor window is placed off your working output".into());
    }
    let before = placement::before();
    let mut cmd = Command::new("niri");
    cmd.env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", &host_socket)
        .env("NIRI_CONFIG", &cfg)
        .env_remove("NIRI_SOCKET")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    die_with_parent(&mut cmd);
    // PR_SET_PDEATHSIG fires when the *thread* that forked exits, so spawn from a
    // thread that lives as long as the test process.
    let (tx, rx) = std::sync::mpsc::channel::<Result<Display, String>>();
    let runtime2 = runtime.clone();
    let root2 = root.clone();
    std::thread::Builder::new()
        .name("niri-parent".into())
        .spawn(move || {
            let mut child: Child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("start nested niri: {e}")));
                    return;
                }
            };
            let deadline = Instant::now() + Duration::from_secs(20);
            let res = loop {
                if let Ok(rd) = std::fs::read_dir(&runtime2) {
                    let found = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"));
                    if let Some(n) = found {
                        break Ok(Display { xdg_runtime_dir: runtime2.clone(), wayland_display: n });
                    }
                }
                if let Ok(Some(st)) = child.try_wait() {
                    let log = std::fs::read_to_string(root2.join("niri.log")).unwrap_or_default();
                    break Err(format!("nested niri exited ({st}): {}", log.chars().take(600).collect::<String>()));
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    break Err("nested niri did not create a Wayland socket in 20 s".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let ok = res.is_ok();
            if ok {
                if let Some(b) = &before {
                    placement::move_away(b, child.id());
                }
            }
            let _ = tx.send(res);
            if ok {
                let _ = child.wait(); // keeps this thread (the PDEATHSIG parent) alive
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

fn detect_display() -> Option<Display> {
    if let Ok(d) = std::env::var("GTK_MCP_TEST_WAYLAND_DISPLAY") {
        let rt = std::env::var("GTK_MCP_TEST_XDG_RUNTIME_DIR")
            .or_else(|_| std::env::var("XDG_RUNTIME_DIR"))
            .unwrap_or_default();
        return Some(Display { xdg_runtime_dir: rt.into(), wayland_display: d });
    }
    match start_nested_niri() {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("gtk-mcp fixture tests SKIPPED: no Wayland display ({e}). Set GTK_MCP_TEST_WAYLAND_DISPLAY (and GTK_MCP_TEST_XDG_RUNTIME_DIR) or run under a host Wayland session with niri installed.");
            None
        }
    }
}

/// The display for this test process, or `None` (tests then skip).
pub fn display() -> Option<Display> {
    DISPLAY.clone()
}

/// Skip the current test with a clear message when no display is available.
#[macro_export]
macro_rules! require_display {
    () => {
        match $crate::fixture_env::display() {
            Some(d) => d,
            None => {
                $crate::fixture_env::note_skip(module_path!());
                return;
            }
        }
    };
}

pub fn note_skip(what: &str) {
    SKIP_NOTE.get_or_init(|| eprintln!("SKIPPED ({what}): no Wayland display available"));
}

pub fn fixture_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    // target/<profile>/deps/<test> -> target/<profile>
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let bin = profile_dir.join("gtk-mcp-fixture");
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args(["build", "-p", "gtk-mcp-fixture"]);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        cmd.env("CARGO_TARGET_DIR", profile_dir.parent().unwrap());
        let st = cmd.status().expect("run cargo build -p gtk-mcp-fixture");
        assert!(st.success(), "building gtk-mcp-fixture failed");
    });
    assert!(bin.exists(), "fixture binary not found at {}", bin.display());
    bin
}

pub struct Fixture {
    pub child: Child,
    pub dir: tempfile::TempDir,
    pub pid: u32,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("fixture stderr at failure:\n{}", self.stderr());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Fixture {
    /// Start the fixture with the agent active. `extra_env` may override anything.
    pub fn start(display: &Display, mutations: bool, extra_env: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(fixture_binary());
        cmd.env("GTK_MCP", "1")
            .env("GTK_MCP_DIR", dir.path())
            .env("XDG_RUNTIME_DIR", &display.xdg_runtime_dir)
            .env("WAYLAND_DISPLAY", &display.wayland_display)
            .env("GDK_BACKEND", "wayland")
            .env("GSETTINGS_BACKEND", "memory")
            .env("GTK_A11Y", "none")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(dir.path().join("fixture.stderr")).unwrap()));
        if mutations {
            cmd.env("GTK_MCP_MUTATIONS", "1");
        }
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        die_with_parent(&mut cmd);
        let child = cmd.spawn().expect("spawn fixture");
        let pid = child.id();
        let mut f = Fixture { child, dir, pid };
        let sock = f.dir.path().join(format!("{pid}.sock"));
        let t = Instant::now();
        while !sock.exists() {
            assert!(t.elapsed() < Duration::from_secs(30), "fixture socket did not appear in 30 s; stderr:\n{}", f.stderr());
            if let Ok(Some(st)) = { let mut c = &mut f.child; c.try_wait() } {
                panic!("fixture exited early ({st}); stderr:\n{}", f.stderr());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        f
    }

    /// Fixture stderr so far (GTK warnings, agent log lines) for failure output.
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("fixture.stderr")).unwrap_or_default()
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.path().join(format!("{}.sock", self.pid))
    }

    pub fn broker(&self, allow_mutations: bool) -> Arc<Broker> {
        let policy = Policy { allow_mutations, ..Policy::default() };
        Broker::new(policy, Config::with_socket_dir(self.dir.path())).unwrap()
    }
}

pub struct Client {
    pub broker: Arc<Broker>,
}

/// Hard cap for any single client call, on top of the broker's own deadlines.
const CALL_WATCHDOG: Duration = Duration::from_secs(45);

impl Client {
    pub async fn call(&self, tool: &str, args: Value) -> Result<ToolOutcome, AgentError> {
        match tokio::time::timeout(CALL_WATCHDOG, self.broker.call_tool(tool, args.clone())).await {
            Ok(r) => r,
            Err(_) => panic!("WATCHDOG: {tool} {args} did not return within {CALL_WATCHDOG:?}"),
        }
    }

    /// Call and parse the first text content as JSON.
    pub async fn json(&self, tool: &str, args: Value) -> Value {
        let out = self.call(tool, args.clone()).await.unwrap_or_else(|e| panic!("{tool} {args} failed: {e} {:?}", e.data));
        out.json_content().unwrap_or_else(|e| panic!("{tool}: no json content: {e}"))
    }

    /// Call and expect an error with this code.
    pub async fn err(&self, tool: &str, args: Value) -> AgentError {
        match self.call(tool, args.clone()).await {
            Ok(_) => panic!("{tool} {args} unexpectedly succeeded"),
            Err(e) => e,
        }
    }

    /// Wait (bounded) until the widget tree stops changing: list views realize rows
    /// lazily during the first frames, which would otherwise race ref comparisons.
    pub async fn settle_tree(&self) {
        let mut last: Vec<String> = Vec::new();
        for _ in 0..40 {
            let s = self.snapshot_json(json!({})).await;
            let mut refs = Vec::new();
            count_refs(&s, &mut refs);
            if refs == last {
                return;
            }
            last = refs;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        panic!("widget tree did not settle within 6 s");
    }

    pub async fn snapshot_json(&self, args: Value) -> Value {
        let mut a = args;
        a["format"] = json!("json");
        self.json("snapshot", a).await
    }
}

pub fn find_node<'a>(v: &'a Value, pred: &dyn Fn(&Value) -> bool) -> Option<&'a Value> {
    if pred(v) {
        return Some(v);
    }
    for c in v["children"].as_array().into_iter().flatten().chain(v["roots"].as_array().into_iter().flatten()) {
        if let Some(n) = find_node(c, pred) {
            return Some(n);
        }
    }
    None
}

pub fn node_by_name<'a>(snap: &'a Value, name: &str) -> &'a Value {
    find_node(snap, &|n| n["identity"]["automation_id"] == name).unwrap_or_else(|| panic!("no node named {name}"))
}

pub fn count_refs(v: &Value, out: &mut Vec<String>) {
    if let Some(r) = v["ref"].as_str() {
        out.push(r.to_string());
    }
    for c in v["children"].as_array().into_iter().flatten().chain(v["roots"].as_array().into_iter().flatten()) {
        count_refs(c, out);
    }
}

pub fn path_exists(p: &Path) -> bool {
    p.exists()
}
