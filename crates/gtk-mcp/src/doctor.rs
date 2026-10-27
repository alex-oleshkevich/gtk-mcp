//! `gtk-mcp doctor`: a read-only diagnosis of the socket directory, every
//! socket in it and the AT-SPI bus. Unlike broker discovery it never unlinks
//! stale sockets or changes anything else.

use crate::session::{HandshakeError, handshake};
use gtk_mcp_protocol::{HelloAck, PROTOCOL_VERSION};
use serde::Serialize;
use serde_json::Value;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct DirReport {
    pub path: PathBuf,
    /// `GTK_MCP_DIR`, `XDG_RUNTIME_DIR`, a fallback, or `--dir`.
    pub source: String,
    pub exists: bool,
    pub is_dir: bool,
    pub owner_uid: Option<u32>,
    pub owned_by_us: Option<bool>,
    /// Permission bits, e.g. `0700`.
    pub mode: Option<String>,
    pub mode_ok: Option<bool>,
    /// Why the directory cannot be used, when it cannot.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SocketStatus {
    Ok,
    /// Dead pid, or nothing listens on the socket.
    Stale,
    /// The agent speaks another protocol version.
    ProtocolMismatch,
    /// The handshake failed or timed out.
    Error,
}

#[derive(Debug, Serialize)]
pub struct SocketReport {
    pub path: PathBuf,
    pub pid: u32,
    pub alive: bool,
    pub status: SocketStatus,
    pub hello: Option<HelloAck>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AtspiReport {
    pub reachable: bool,
    pub address: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub socket_dir: DirReport,
    pub sockets: Vec<SocketReport>,
    pub atspi: AtspiReport,
    pub healthy: bool,
}

/// Owner and mode of the directory, and whether it can be used.
pub fn check_dir(path: &Path, source: &str) -> DirReport {
    let mut r = DirReport {
        path: path.to_owned(),
        source: source.to_owned(),
        exists: false,
        is_dir: false,
        owner_uid: None,
        owned_by_us: None,
        mode: None,
        mode_ok: None,
        problem: None,
    };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        // Agents create the directory on first start, so a missing one is fine.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return r,
        Err(e) => {
            r.problem = Some(format!("cannot stat: {e}"));
            return r;
        }
    };
    r.exists = true;
    r.is_dir = meta.is_dir();
    let uid = meta.uid();
    let mode = meta.permissions().mode() & 0o777;
    // SAFETY: plain query.
    let ours = unsafe { libc::geteuid() };
    r.owner_uid = Some(uid);
    r.owned_by_us = Some(uid == ours);
    r.mode = Some(format!("{mode:04o}"));
    r.mode_ok = Some(mode & 0o077 == 0);
    let mut problems = Vec::new();
    if !r.is_dir {
        problems.push("not a directory".to_owned());
    } else if let Err(e) = std::fs::read_dir(path) {
        problems.push(format!("cannot read: {e}"));
    }
    if uid != ours {
        problems.push(format!("owned by uid {uid}, not by us (uid {ours})"));
    }
    if mode & 0o077 != 0 {
        problems.push(format!("mode {mode:04o} lets other users in; expected 0700"));
    }
    if !problems.is_empty() {
        r.problem = Some(problems.join("; "));
    }
    r
}

fn pid_alive(pid: u32) -> bool {
    let Ok(p) = i32::try_from(pid) else { return false };
    if p == 0 {
        return false;
    }
    // SAFETY: signal 0 only checks existence. EPERM means it exists, owned by someone else.
    unsafe { libc::kill(p, 0) == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) }
}

/// Probe one `<pid>.sock` without touching it.
pub fn check_socket(path: &Path, pid: u32, timeout: Duration) -> SocketReport {
    let alive = pid_alive(pid);
    let mut r = SocketReport { path: path.to_owned(), pid, alive, status: SocketStatus::Ok, hello: None, error: None };
    if !alive {
        r.status = SocketStatus::Stale;
        r.error = Some(format!("process {pid} is not running"));
        return r;
    }
    match handshake(path, pid, timeout) {
        Ok(h) => {
            if h.ack.protocol != PROTOCOL_VERSION {
                r.status = SocketStatus::ProtocolMismatch;
                r.error = Some(format!(
                    "agent speaks protocol {}, this broker speaks {PROTOCOL_VERSION}",
                    h.ack.protocol
                ));
            }
            r.hello = Some(h.ack);
        }
        Err(HandshakeError::Refused) => {
            r.status = SocketStatus::Stale;
            r.error = Some("connection refused: nothing listens on the socket".into());
        }
        Err(HandshakeError::Gone) => {
            r.status = SocketStatus::Error;
            r.error = Some("socket vanished during the check".into());
        }
        Err(HandshakeError::Timeout) => {
            r.status = SocketStatus::Error;
            r.error = Some(format!("no hello_ack within {timeout:?}"));
        }
        Err(HandshakeError::Other(e)) => {
            r.status = SocketStatus::Error;
            r.error = Some(e);
        }
    }
    r
}

/// Whether `org.a11y.Bus.GetAddress` answers on the session bus.
pub async fn check_atspi(timeout: Duration) -> AtspiReport {
    let fail = |e: String| AtspiReport { reachable: false, address: None, error: Some(e) };
    let conn = match tokio::time::timeout(timeout, zbus::Connection::session()).await {
        Err(_) => return fail(format!("connecting to the session bus timed out after {timeout:?}")),
        Ok(Err(e)) => return fail(format!("cannot connect to the session bus: {e}")),
        Ok(Ok(c)) => c,
    };
    let call = conn.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &());
    match tokio::time::timeout(timeout, call).await {
        Err(_) => fail(format!("org.a11y.Bus.GetAddress timed out after {timeout:?} (is at-spi-bus-launcher running?)")),
        Ok(Err(e)) => fail(format!("org.a11y.Bus.GetAddress failed: {e}")),
        Ok(Ok(msg)) => match msg.body().deserialize::<String>() {
            Ok(a) => AtspiReport { reachable: true, address: Some(a), error: None },
            Err(e) => fail(format!("cannot decode the GetAddress reply: {e}")),
        },
    }
}

/// Run every check. `healthy` is false when the directory is unusable or a socket
/// fails its handshake or speaks another protocol; stale sockets and an
/// unreachable AT-SPI bus are reported but do not make it unhealthy.
pub async fn run(dir: &Path, source: &str, handshake_timeout: Duration) -> Report {
    let socket_dir = check_dir(dir, source);
    let mut sockets = Vec::new();
    if socket_dir.is_dir {
        let dir = dir.to_owned();
        sockets = tokio::task::spawn_blocking(move || {
            let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let pid: u32 = e.file_name().to_str()?.strip_suffix(".sock")?.parse().ok()?;
                    Some((pid, e.path()))
                })
                .collect();
            found.sort();
            found.into_iter().map(|(pid, path)| check_socket(&path, pid, handshake_timeout)).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
    }
    let atspi = check_atspi(Duration::from_secs(3)).await;
    let healthy = socket_dir.problem.is_none()
        && sockets.iter().all(|s| matches!(s.status, SocketStatus::Ok | SocketStatus::Stale));
    Report { socket_dir, sockets, atspi, healthy }
}

fn version(v: [u32; 3]) -> String {
    format!("{}.{}.{}", v[0], v[1], v[2])
}

impl Report {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("Report serializes")
    }

    /// The human-readable report.
    pub fn render(&self) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let d = &self.socket_dir;
        let _ = writeln!(o, "socket directory: {} (from {})", d.path.display(), d.source);
        if !d.exists {
            let _ = writeln!(o, "  does not exist yet (agents create it when an app starts); no apps");
        } else {
            let _ = writeln!(
                o,
                "  exists: yes, directory: {}, owner uid: {}{}, mode: {}{}",
                if d.is_dir { "yes" } else { "no" },
                d.owner_uid.map_or("?".into(), |u| u.to_string()),
                if d.owned_by_us == Some(true) { " (ours)" } else { " (NOT ours)" },
                d.mode.as_deref().unwrap_or("?"),
                if d.mode_ok == Some(true) { "" } else { " (expected 0700)" },
            );
        }
        if let Some(p) = &d.problem {
            let _ = writeln!(o, "  PROBLEM: {p}");
        }
        let _ = writeln!(o);
        if self.sockets.is_empty() {
            let _ = writeln!(o, "sockets: none (no instrumented apps are running)");
        } else {
            let _ = writeln!(o, "sockets: {}", self.sockets.len());
        }
        for s in &self.sockets {
            let name = s.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let _ = writeln!(o, "  {name}: pid {} {}", s.pid, if s.alive { "alive" } else { "dead" });
            match (&s.hello, s.status) {
                (Some(h), status) => {
                    let _ = writeln!(o, "    handshake: {}", if status == SocketStatus::Ok { "ok" } else { "protocol mismatch" });
                    let _ = writeln!(o, "    protocol: {} (broker: {PROTOCOL_VERSION})", h.protocol);
                    let _ = writeln!(o, "    agent version: {}", h.agent_version);
                    let _ = writeln!(o, "    app id: {}", h.app_id.as_deref().unwrap_or("-"));
                    let _ = writeln!(o, "    GTK: {}", version(h.gtk_version));
                    let _ = writeln!(o, "    libadwaita: {}", h.adw_version.map_or("-".into(), version));
                    let caps: Vec<String> = h
                        .capabilities
                        .iter()
                        .map(|c| serde_json::to_value(c).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default())
                        .collect();
                    let _ = writeln!(o, "    capabilities: {}", if caps.is_empty() { "-".into() } else { caps.join(", ") });
                }
                (None, SocketStatus::Stale) => {
                    let _ = writeln!(o, "    STALE: {}", s.error.as_deref().unwrap_or(""));
                    let _ = writeln!(o, "    left in place; the broker removes stale sockets when it next scans");
                }
                (None, _) => {
                    let _ = writeln!(o, "    handshake FAILED: {}", s.error.as_deref().unwrap_or("unknown error"));
                }
            }
            if s.status == SocketStatus::ProtocolMismatch {
                let _ = writeln!(o, "    PROBLEM: {}", s.error.as_deref().unwrap_or(""));
            }
        }
        let _ = writeln!(o);
        match (&self.atspi.reachable, &self.atspi.address, &self.atspi.error) {
            (true, Some(a), _) => {
                let _ = writeln!(o, "AT-SPI bus: reachable ({a})");
            }
            (_, _, e) => {
                let _ = writeln!(o, "AT-SPI bus: not reachable: {}", e.as_deref().unwrap_or("unknown error"));
                let _ = writeln!(o, "  only the a11y toolset and --allow-atspi need it");
            }
        }
        let _ = writeln!(o);
        let _ = writeln!(o, "{}", if self.healthy { "healthy" } else { "UNHEALTHY" });
        o
    }
}
