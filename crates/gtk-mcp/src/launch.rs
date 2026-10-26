//! Broker-launched apps (`docs/plan.md` 4.5, 5.5): spawn argv without a shell in
//! its own process group with the GTK presets, capture stdout/stderr into a
//! cursor log, track the exit status, and stop the group (SIGTERM, then SIGKILL).

use crate::ring::Ring;
use gtk_mcp_protocol::{AgentError, ErrorCode, LaunchAppParams, LaunchPresets};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Longest captured log line; the rest of an over-long line is dropped.
const MAX_LINE: usize = 16 * 1024;
/// How long the exit status waits for the output pipes to drain, so that the
/// last lines (e.g. a panic message) are in the log before the exit is visible.
const DRAIN_GRACE: Duration = Duration::from_millis(500);
/// How often a reaped launch's process group is probed until it empties.
/// Reusing that id needs the pid counter to wrap around, which takes far longer.
const GROUP_POLL: Duration = Duration::from_millis(50);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A validated `launch_app` request; `restart_app` replays it.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub presets: LaunchPresets,
    pub wayland_display: Option<String>,
    pub xdg_runtime_dir: Option<String>,
}

fn bad(msg: impl Into<String>) -> AgentError {
    AgentError::invalid_params(msg)
}

fn check_text(what: &str, s: &str) -> Result<(), AgentError> {
    if s.contains('\0') {
        return Err(bad(format!("{what} must not contain NUL")));
    }
    Ok(())
}

impl LaunchSpec {
    pub fn from_params(p: LaunchAppParams) -> Result<Self, AgentError> {
        if p.argv.is_empty() || p.argv[0].is_empty() {
            return Err(bad("`argv` needs at least the program"));
        }
        for a in &p.argv {
            check_text("argv entries", a)?;
        }
        let env = p.env.unwrap_or_default();
        for (k, v) in &env {
            if k.to_ascii_uppercase().starts_with("GTK_MCP") {
                return Err(bad(format!(
                    "env '{k}' is not allowed: GTK_MCP* variables are set by the broker from its policy (--allow-mutations)"
                )));
            }
            if k.is_empty() || k.contains('=') {
                return Err(bad(format!("env name '{k}' is invalid")));
            }
            check_text("env names and values", k)?;
            check_text("env names and values", v)?;
        }
        let presets = p.presets.unwrap_or_default();
        for (what, v) in [
            ("presets.language", &presets.language),
            ("presets.gsk_renderer", &presets.gsk_renderer),
            ("presets.gtk_debug", &presets.gtk_debug),
            ("wayland_display", &p.wayland_display),
            ("xdg_runtime_dir", &p.xdg_runtime_dir),
        ] {
            if let Some(v) = v {
                check_text(what, v)?;
            }
        }
        let cwd = p.cwd.map(PathBuf::from);
        if let Some(c) = &cwd {
            if !c.is_dir() {
                return Err(bad(format!("`cwd` '{}' is not a directory", c.display())));
            }
        }
        Ok(Self { argv: p.argv, cwd, env, presets, wayland_display: p.wayland_display, xdg_runtime_dir: p.xdg_runtime_dir })
    }

    /// The environment the child gets on top of the broker's own: variables to
    /// set and variables to remove. `GTK_MCP*` inherited from the broker is
    /// removed; the broker's policy alone decides `GTK_MCP_MUTATIONS`.
    pub fn env_plan(&self, socket_dir: &Path, allow_mutations: bool, inherited: &[String]) -> EnvPlan {
        let mut set: BTreeMap<String, String> = self.env.clone();
        let mut merge = |name: &str, token: &str| {
            let cur = set.entry(name.to_owned()).or_default();
            if !cur.split([',', ':', ';', ' ']).any(|t| t == token) {
                if !cur.is_empty() {
                    cur.push(',');
                }
                cur.push_str(token);
            }
        };
        if self.presets.no_portals == Some(true) {
            merge("GDK_DEBUG", "no-portals");
        }
        if self.presets.instance_count == Some(true) {
            merge("GOBJECT_DEBUG", "instance-count");
        }
        if let Some(v) = &self.presets.gtk_debug {
            set.insert("GTK_DEBUG".into(), v.clone());
        }
        if let Some(v) = self.presets.a11y {
            set.insert("GTK_A11Y".into(), v.as_str().into());
        }
        if let Some(v) = &self.presets.language {
            set.insert("LANGUAGE".into(), v.clone());
        }
        if let Some(v) = &self.presets.gsk_renderer {
            set.insert("GSK_RENDERER".into(), v.clone());
        }
        if let Some(v) = &self.wayland_display {
            set.insert("WAYLAND_DISPLAY".into(), v.clone());
        }
        if let Some(v) = &self.xdg_runtime_dir {
            set.insert("XDG_RUNTIME_DIR".into(), v.clone());
        }
        set.insert("GTK_MCP".into(), "1".into());
        set.insert("GTK_MCP_DIR".into(), socket_dir.to_string_lossy().into_owned());
        if allow_mutations {
            set.insert("GTK_MCP_MUTATIONS".into(), "1".into());
        }
        let remove = inherited.iter().filter(|k| k.starts_with("GTK_MCP") && !set.contains_key(*k)).cloned().collect();
        EnvPlan { set, remove }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvPlan {
    pub set: BTreeMap<String, String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn to_json(self) -> Value {
        json!({ "code": self.code, "signal": self.signal })
    }
}

struct Status {
    exit: Option<ExitInfo>,
    /// Output pipes still open.
    readers: u8,
}

/// One process the broker launched, from spawn until the broker drops it.
pub struct Launch {
    pid: u32,
    pub spec: LaunchSpec,
    /// Captured stdout/stderr lines: `{"source": "stdout"|"stderr", "text", "time_ms"}`.
    pub logs: Arc<Mutex<Ring>>,
    status: Mutex<Status>,
    cv: Condvar,
    /// The child was reaped (set before `exit` is published).
    reaped: AtomicBool,
    /// The process group was seen empty after the leader was reaped. From then
    /// on its id may belong to an unrelated group and is never signalled.
    group_gone: AtomicBool,
    /// A session adopted this launch.
    attached: AtomicBool,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn pump(mut r: impl Read, source: &'static str, launch: Arc<Launch>) {
    let mut buf = [0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    let mut over = false;
    let flush = |line: &mut Vec<u8>, over: &mut bool| {
        let mut text = String::from_utf8_lossy(line).into_owned();
        if *over {
            text.push_str(" [line truncated]");
        }
        lock(&launch.logs).push(json!({ "source": source, "text": text, "time_ms": now_ms() }));
        line.clear();
        *over = false;
    };
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                flush(&mut line, &mut over);
            } else if line.len() < MAX_LINE {
                line.push(b);
            } else {
                over = true;
            }
        }
    }
    if !line.is_empty() {
        flush(&mut line, &mut over);
    }
    let mut st = lock(&launch.status);
    st.readers = st.readers.saturating_sub(1);
    launch.cv.notify_all();
}

impl Launch {
    /// Spawn `spec` in a new process group. The caller supplies the policy-derived environment.
    pub fn spawn(spec: LaunchSpec, plan: &EnvPlan, ring_capacity: usize) -> Result<Arc<Launch>, AgentError> {
        let mut cmd = Command::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
        if let Some(c) = &spec.cwd {
            cmd.current_dir(c);
        }
        for k in &plan.remove {
            cmd.env_remove(k);
        }
        cmd.envs(&plan.set);
        let mut child = cmd.spawn().map_err(|e| {
            bad(format!("cannot execute '{}': {e}", spec.argv[0]))
        })?;
        let launch = Arc::new(Launch {
            pid: child.id(),
            spec,
            logs: Arc::new(Mutex::new(Ring::new(ring_capacity))),
            status: Mutex::new(Status { exit: None, readers: 2 }),
            cv: Condvar::new(),
            reaped: AtomicBool::new(false),
            group_gone: AtomicBool::new(false),
            attached: AtomicBool::new(false),
        });
        let out = child.stdout.take().expect("piped stdout");
        let err = child.stderr.take().expect("piped stderr");
        for (r, name) in [(Box::new(out) as Box<dyn Read + Send>, "stdout"), (Box::new(err), "stderr")] {
            let l = launch.clone();
            std::thread::Builder::new()
                .name(format!("gtk-mcp-{name}-{}", launch.pid))
                .spawn(move || pump(r, name, l))
                .expect("spawn log pump");
        }
        let l = launch.clone();
        std::thread::Builder::new()
            .name(format!("gtk-mcp-wait-{}", launch.pid))
            .spawn(move || {
                let status = child.wait();
                l.reaped.store(true, Ordering::Release);
                let exit = match status {
                    Ok(s) => ExitInfo { code: s.code(), signal: s.signal() },
                    Err(_) => ExitInfo { code: None, signal: None },
                };
                // Let the pumps drain what the process wrote before it died.
                let deadline = Instant::now() + DRAIN_GRACE;
                let mut st = lock(&l.status);
                while st.readers > 0 {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    st = l.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
                }
                st.exit = Some(exit);
                l.cv.notify_all();
                drop(st);
                // Watch the group until it empties and latch that, so its id is
                // never signalled after it could have been reused. Weak: the
                // watch does not keep a dropped launch alive.
                let weak = Arc::downgrade(&l);
                drop(l);
                while let Some(l) = weak.upgrade() {
                    if !l.group_alive() {
                        break;
                    }
                    drop(l);
                    std::thread::sleep(GROUP_POLL);
                }
            })
            .expect("spawn waiter");
        Ok(launch)
    }

    /// Process id of the child, which is also its process group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn exit(&self) -> Option<ExitInfo> {
        lock(&self.status).exit
    }

    /// The child has been reaped (stronger than "no longer running": its pid
    /// may be reused, so the group is never signalled afterwards).
    pub fn reaped(&self) -> bool {
        self.reaped.load(Ordering::Acquire)
    }

    /// Block until the exit status is known, at most `timeout`.
    pub fn wait_exit(&self, timeout: Duration) -> Option<ExitInfo> {
        let deadline = Instant::now() + timeout;
        let mut st = lock(&self.status);
        while st.exit.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = self.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        st.exit
    }

    /// First claim wins: a launch belongs to one session.
    pub(crate) fn claim(&self) -> bool {
        !self.attached.swap(true, Ordering::AcqRel)
    }

    /// The process group still has members (the leader may be gone already).
    /// A group with members keeps its id reserved, so a successful probe after
    /// the leader was reaped means our remaining members. Once a reaped
    /// launch's group is seen empty that is latched: the id may be reused by an
    /// unrelated group, which must never be reported (or signalled) as ours.
    pub fn group_alive(&self) -> bool {
        if self.group_gone.load(Ordering::Acquire) {
            return false;
        }
        // Read before probing: an empty group only counts once the leader is reaped.
        let reaped = self.reaped();
        // SAFETY: signal 0 only probes.
        if unsafe { libc::kill(-(self.pid as i32), 0) } == 0 {
            return true;
        }
        if reaped {
            self.group_gone.store(true, Ordering::Release);
        }
        false
    }

    /// Signal the whole process group; `false` when it is already gone.
    pub fn signal_group(&self, sig: i32) -> bool {
        if !self.group_alive() {
            return false;
        }
        // SAFETY: the group was created by this launch and still has members.
        unsafe { libc::kill(-(self.pid as i32), sig) == 0 }
    }

    /// Last `n` captured lines as JSON entries.
    pub fn log_tail(&self, n: usize) -> Vec<Value> {
        let g = lock(&self.logs);
        let next = g.since(u64::MAX).next_cursor;
        g.since(next.saturating_sub(n as u64)).entries.into_iter().map(|e| e.value).collect()
    }

    pub fn info_json(&self) -> Value {
        json!({
            "pid": self.pid,
            "process_group": self.pid,
            "argv": self.spec.argv,
            "cwd": self.spec.cwd,
            "exit": self.exit().map(ExitInfo::to_json),
        })
    }
}

/// SIGTERM the group, wait up to `kill_after` for it to disappear, then SIGKILL
/// and wait up to 2 s more. Returns whether SIGKILL was needed.
pub async fn stop(launch: &Launch, kill_after: Duration) -> bool {
    async fn wait_gone(l: &Launch, within: Duration) -> bool {
        let end = Instant::now() + within;
        loop {
            if l.reaped() && !l.group_alive() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    if launch.reaped() && !launch.group_alive() {
        return false;
    }
    launch.signal_group(libc::SIGTERM);
    if wait_gone(launch, kill_after).await {
        return false;
    }
    launch.signal_group(libc::SIGKILL);
    wait_gone(launch, Duration::from_secs(2)).await;
    true
}

/// Synchronous best-effort termination of every still-running launch (the
/// broker is going away): SIGTERM to all groups, SIGKILL after 500 ms.
pub fn terminate_all(launches: &[Arc<Launch>]) {
    let live: Vec<&Arc<Launch>> = launches.iter().filter(|l| l.group_alive()).collect();
    for l in &live {
        l.signal_group(libc::SIGTERM);
    }
    let end = Instant::now() + Duration::from_millis(500);
    while Instant::now() < end && live.iter().any(|l| l.group_alive()) {
        std::thread::sleep(Duration::from_millis(20));
    }
    for l in live {
        l.signal_group(libc::SIGKILL);
    }
}

pub fn exited_error(launch: &Launch, what: &str) -> AgentError {
    let exit = launch.exit();
    AgentError::new(ErrorCode::SessionExited, what.to_owned()).with_data(json!({
        "code": exit.and_then(|e| e.code),
        "signal": exit.and_then(|e| e.signal),
        "logs": launch.log_tail(20),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(argv: &[&str]) -> LaunchAppParams {
        LaunchAppParams {
            session: String::new(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: None,
            presets: None,
            wayland_display: None,
            xdg_runtime_dir: None,
            timeout_ms: None,
        }
    }

    #[test]
    fn rejects_gtk_mcp_env_any_case() {
        for k in ["GTK_MCP", "GTK_MCP_MUTATIONS", "GTK_MCP_DIR", "gtk_mcp_x"] {
            let mut p = params(&["app"]);
            p.env = Some([(k.to_owned(), "1".to_owned())].into());
            let e = LaunchSpec::from_params(p).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidParams, "{k}");
        }
        assert!(LaunchSpec::from_params(params(&[])).is_err());
        assert!(LaunchSpec::from_params(params(&["a\0b"])).is_err());
    }

    #[test]
    fn env_plan_presets_and_policy() {
        let mut p = params(&["app"]);
        p.env = Some([("GDK_DEBUG".to_owned(), "vulkan".to_owned()), ("FOO".to_owned(), "1".to_owned())].into());
        p.presets = Some(LaunchPresets {
            no_portals: Some(true),
            instance_count: Some(true),
            language: Some("de".into()),
            gsk_renderer: Some("cairo".into()),
            gtk_debug: Some("layout".into()),
            a11y: Some(gtk_mcp_protocol::A11yPreset::None),
        });
        p.wayland_display = Some("wayland-9".into());
        let spec = LaunchSpec::from_params(p).unwrap();
        let plan = spec.env_plan(Path::new("/run/x"), false, &["GTK_MCP_MUTATIONS".into(), "HOME".into()]);
        assert_eq!(plan.set["GDK_DEBUG"], "vulkan,no-portals");
        assert_eq!(plan.set["GOBJECT_DEBUG"], "instance-count");
        assert_eq!(plan.set["GTK_A11Y"], "none");
        assert_eq!(plan.set["LANGUAGE"], "de");
        assert_eq!(plan.set["GSK_RENDERER"], "cairo");
        assert_eq!(plan.set["GTK_DEBUG"], "layout");
        assert_eq!(plan.set["WAYLAND_DISPLAY"], "wayland-9");
        assert_eq!(plan.set["GTK_MCP"], "1");
        assert_eq!(plan.set["GTK_MCP_DIR"], "/run/x");
        assert!(!plan.set.contains_key("GTK_MCP_MUTATIONS"));
        assert_eq!(plan.remove, vec!["GTK_MCP_MUTATIONS".to_owned()]);
        let plan = spec.env_plan(Path::new("/run/x"), true, &["GTK_MCP_MUTATIONS".into()]);
        assert_eq!(plan.set["GTK_MCP_MUTATIONS"], "1");
        assert!(plan.remove.is_empty());
    }

    /// gm-3n1: once the reaped launch's group empties, the waiter latches it
    /// on its own (no caller has to probe first), and the id is never
    /// signalled again even if an unrelated group took it over.
    #[test]
    fn emptied_group_is_latched_and_never_signalled() {
        let spec = LaunchSpec::from_params(params(&["sh", "-c", "sleep 1.5 >/dev/null 2>&1 & exit 0"])).unwrap();
        let plan = spec.env_plan(Path::new("/nonexistent"), false, &[]);
        let l = Launch::spawn(spec, &plan, 16).unwrap();
        assert_eq!(l.wait_exit(Duration::from_secs(5)).and_then(|e| e.code), Some(0));
        assert!(l.reaped());
        assert!(l.group_alive(), "the background sleep keeps the group");
        let end = Instant::now() + Duration::from_secs(5);
        while !l.group_gone.load(Ordering::Acquire) {
            assert!(Instant::now() < end, "the emptied group was never latched");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!l.group_alive());
        assert!(!l.signal_group(0));
    }
}
