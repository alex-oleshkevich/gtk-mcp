//! Agent configuration (`docs/plan.md` 4.5, 7.1).
//!
//! The only gate is compile time: code that calls `attach` gets an active
//! agent. The sole environment input is `GTK_MCP_DIR`, the socket location.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Socket directory: `GTK_MCP_DIR`, else `$XDG_RUNTIME_DIR/gtk-mcp`.
    pub dir: PathBuf,
    /// `GOBJECT_DEBUG` contained `instance-count` when the agent attached, so
    /// `g_type_get_instance_count` is live (`Capability::InstanceCount`).
    pub instance_count: bool,
}

impl Config {
    /// Build the configuration from the process environment. Returns `None`
    /// only when no socket directory can be determined (`GTK_MCP_DIR` unset
    /// and no `XDG_RUNTIME_DIR`).
    pub fn from_env() -> Option<Config> {
        let vars: Vec<(String, String)> =
            std::env::vars().filter(|(k, _)| k == "GTK_MCP_DIR").collect();
        let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        let instance_count = std::env::var("GOBJECT_DEBUG").is_ok_and(|v| gobject_debug_has_instance_count(&v));
        Self::from_vars(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())), xdg.as_deref())
            .map(|c| Config { instance_count, ..c })
    }

    /// Pure parsing step of [`Config::from_env`].
    pub fn from_vars<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>, xdg_runtime_dir: Option<&str>) -> Option<Config> {
        let mut dir = None;
        for (k, v) in vars {
            if k == "GTK_MCP_DIR" && !v.is_empty() {
                dir = Some(PathBuf::from(v));
            }
        }
        let dir = dir.or_else(|| xdg_runtime_dir.map(|x| PathBuf::from(x).join("gtk-mcp")))?;
        Some(Config { dir, instance_count: false })
    }
}

/// GLib splits `GOBJECT_DEBUG` on `:`, `;`, `,` and spaces.
pub fn gobject_debug_has_instance_count(value: &str) -> bool {
    value.split([':', ';', ',', ' ', '\t']).any(|t| t == "instance-count")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_with_no_env_vars() {
        // No GTK_MCP* variables at all: still active, default socket dir.
        let c = Config::from_vars([], Some("/run/user/1")).unwrap();
        assert_eq!(c, Config { dir: "/run/user/1/gtk-mcp".into(), instance_count: false });
    }

    #[test]
    fn instance_count_tokens() {
        assert!(gobject_debug_has_instance_count("instance-count"));
        assert!(gobject_debug_has_instance_count("signals:instance-count"));
        assert!(gobject_debug_has_instance_count("objects instance-count"));
        assert!(!gobject_debug_has_instance_count("objects"));
        assert!(!gobject_debug_has_instance_count("my-instance-count"));
    }

    #[test]
    fn parses_and_defaults() {
        let c = Config::from_vars([("GTK_MCP_DIR", "/tmp/x")], None).unwrap();
        assert_eq!(c, Config { dir: "/tmp/x".into(), instance_count: false });
        // no directory at all: cannot bind rather than guessing one
        assert_eq!(Config::from_vars([], None), None);
    }
}
