//! Startup policy (`docs/plan.md` 4.5, 7.1) and runtime configuration.

use gtk_mcp_protocol::{TOOLS, ToolSpec};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// When set, only these tools.
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Vec<String>,
    pub allow_launch: bool,
    /// Enables the AT-SPI source (AT-SPI events, the `a11y` tools' data).
    pub allow_atspi: bool,
}

impl Policy {
    /// Tools exposed by this policy, in registry order. Unknown tool names
    /// are errors, so typos fail at startup instead of silently hiding tools.
    pub fn enabled_tools(&self) -> Result<Vec<&'static ToolSpec>, String> {
        let registered = |n: &str| TOOLS.iter().any(|t| t.name == n);
        let list = |names: &[String], flag: &str| -> Result<(), String> {
            for n in names {
                if !registered(n) {
                    return Err(format!(
                        "unknown tool '{n}' in {flag} (registered: {})",
                        TOOLS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            Ok(())
        };
        if let Some(t) = &self.tools {
            list(t, "--tools")?;
        }
        list(&self.exclude_tools, "--exclude-tools")?;
        Ok(TOOLS
            .iter()
            .filter(|t| self.tools.as_ref().is_none_or(|only| only.iter().any(|n| n == t.name)))
            .filter(|t| !self.exclude_tools.iter().any(|n| n == t.name))
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub socket_dir: PathBuf,
    pub retain_snapshots: usize,
    pub retain_captures: usize,
    pub retain_exited: usize,
    /// Deadline for tools without their own formula (`docs/plan.md` 5.2).
    pub default_deadline_ms: u64,
    /// Per-connection handshake bound so a hung app cannot hang `list_apps`.
    pub handshake_timeout: Duration,
    /// Capacity of each session's event and log ring.
    pub ring_capacity: usize,
    /// Where the AT-SPI client finds the buses (default: the environment).
    pub atspi_bus: crate::a11y::atspi::BusConfig,
}

impl Config {
    pub fn default_socket_dir() -> PathBuf {
        Self::socket_dir_source().0
    }

    /// The default socket directory and where it came from.
    pub fn socket_dir_source() -> (PathBuf, &'static str) {
        if let Some(d) = std::env::var_os("GTK_MCP_DIR").filter(|d| !d.is_empty()) {
            return (PathBuf::from(d), "GTK_MCP_DIR");
        }
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            Some(d) => (PathBuf::from(d).join("gtk-mcp"), "XDG_RUNTIME_DIR"),
            None => (
                PathBuf::from(format!("/tmp/gtk-mcp-{}", unsafe { libc::getuid() })),
                "fallback (neither GTK_MCP_DIR nor XDG_RUNTIME_DIR is set)",
            ),
        }
    }

    pub fn with_socket_dir(dir: impl Into<PathBuf>) -> Self {
        Self { socket_dir: dir.into(), ..Self::default() }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket_dir: Self::default_socket_dir(),
            retain_snapshots: 16,
            retain_captures: 16,
            retain_exited: 8,
            default_deadline_ms: 10_000,
            handshake_timeout: Duration::from_secs(3),
            ring_capacity: 1024,
            atspi_bus: Default::default(),
        }
    }
}
