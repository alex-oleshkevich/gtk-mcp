//! Agent configuration from the environment (`docs/plan.md` 4.5, 7.1).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `GTK_MCP_MUTATIONS=1`: the agent accepts mutating tools.
    pub mutations: bool,
    /// Socket directory: `GTK_MCP_DIR`, else `$XDG_RUNTIME_DIR/gtk-mcp`.
    pub dir: PathBuf,
    /// `GOBJECT_DEBUG` contained `instance-count` when the agent attached, so
    /// `g_type_get_instance_count` is live (`Capability::InstanceCount`).
    pub instance_count: bool,
}

impl Config {
    /// Read `GTK_MCP*` from the process environment and remove all of them, so
    /// that apps the instrumented process launches are not instrumented
    /// (plan 4.5). Returns `None` unless `GTK_MCP=1`.
    pub fn take_from_env() -> Option<Config> {
        let vars: Vec<(String, String)> =
            std::env::vars().filter(|(k, _)| k.starts_with("GTK_MCP")).collect();
        let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        let instance_count = std::env::var("GOBJECT_DEBUG").is_ok_and(|v| gobject_debug_has_instance_count(&v));
        for (k, _) in &vars {
            // SAFETY: `attach` runs on the main thread during start-up, before
            // the agent spawns any thread; documented requirement of `attach`.
            unsafe { std::env::remove_var(k) };
        }
        Self::from_vars(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())), xdg.as_deref())
            .map(|c| Config { instance_count, ..c })
    }

    /// Pure parsing step of [`Config::take_from_env`].
    pub fn from_vars<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>, xdg_runtime_dir: Option<&str>) -> Option<Config> {
        let (mut active, mut mutations, mut dir) = (false, false, None);
        for (k, v) in vars {
            match k {
                "GTK_MCP" => active = v == "1",
                "GTK_MCP_MUTATIONS" => mutations = v == "1",
                "GTK_MCP_DIR" if !v.is_empty() => dir = Some(PathBuf::from(v)),
                _ => {}
            }
        }
        if !active {
            return None;
        }
        let dir = dir.or_else(|| xdg_runtime_dir.map(|x| PathBuf::from(x).join("gtk-mcp")))?;
        Some(Config { mutations, dir, instance_count: false })
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
    fn inert_without_gtk_mcp_1() {
        assert_eq!(Config::from_vars([("GTK_MCP_MUTATIONS", "1")], Some("/run/user/1")), None);
        assert_eq!(Config::from_vars([("GTK_MCP", "0")], Some("/run/user/1")), None);
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
        let c = Config::from_vars([("GTK_MCP", "1")], Some("/run/user/1")).unwrap();
        assert_eq!(c, Config { mutations: false, dir: "/run/user/1/gtk-mcp".into(), instance_count: false });
        let c = Config::from_vars([("GTK_MCP", "1"), ("GTK_MCP_MUTATIONS", "1"), ("GTK_MCP_DIR", "/tmp/x")], None).unwrap();
        assert_eq!(c, Config { mutations: true, dir: "/tmp/x".into(), instance_count: false });
        // no directory at all: inert rather than guessing one
        assert_eq!(Config::from_vars([("GTK_MCP", "1")], None), None);
    }
}
