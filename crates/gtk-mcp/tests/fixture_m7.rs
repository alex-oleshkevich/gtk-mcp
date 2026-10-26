//! M7 acceptance: the C ABI (`gtk-mcp-agent-capi`), its header and the GtkMcp-1
//! typelib, against real C / Python (PyGObject) / GJS apps that attach the agent
//! and are snapshotted through the real broker. Every wait is bounded.
//!
//! The typelib is compiled from `GtkMcp-1.gir` with `g-ir-compiler`: the one on
//! `PATH`, or the one in the Flatpak `org.freedesktop.Sdk` runtime when the host
//! has no gobject-introspection tools. If neither exists the tests fail with
//! that diagnosis (never skip).

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SMOKE_TIMEOUT: Duration = Duration::from_secs(60);
const GIR_CLOSURE: &[&str] = &[
    "Gtk-4.0", "Gio-2.0", "GObject-2.0", "GLib-2.0", "Gdk-4.0", "Gsk-4.0", "Pango-1.0", "GdkPixbuf-2.0", "Graphene-1.0",
    "HarfBuzz-0.0", "PangoCairo-1.0", "GModule-2.0",
];

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gtk-mcp-agent-capi")
}

struct Artifacts {
    /// Directory with `libgtk_mcp_agent_capi.so`, `GtkMcp-1.typelib`, `smoke-c`.
    out: PathBuf,
    lib: PathBuf,
}

fn run(cmd: &mut Command, what: &str) -> String {
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: cannot run: {e}"));
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{what} failed ({}):\n{text}", out.status);
    text
}

fn have(bin: &str) -> bool {
    Command::new(bin).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Compile `GtkMcp-1.gir` into `out/GtkMcp-1.typelib`.
fn compile_typelib(out: &Path) {
    let gir = crate_dir().join("GtkMcp-1.gir");
    let typelib = out.join("GtkMcp-1.typelib");
    let _ = std::fs::remove_file(&typelib);
    if have("g-ir-compiler") {
        run(
            Command::new("g-ir-compiler")
                .arg("--includedir=/usr/share/gir-1.0")
                .arg("--shared-library=libgtk_mcp_agent_capi.so")
                .arg(&gir)
                .arg("-o")
                .arg(&typelib),
            "g-ir-compiler",
        );
    } else {
        // Flatpak SDK fallback: stage the dependency GIRs where the sandbox can see them.
        assert!(
            have("flatpak"),
            "no `g-ir-compiler` on PATH and no `flatpak` (install gobject-introspection, or the org.freedesktop.Sdk//25.08 flatpak runtime)"
        );
        let stage = out.join("gir");
        std::fs::create_dir_all(&stage).unwrap();
        for n in GIR_CLOSURE {
            let from = Path::new("/usr/share/gir-1.0").join(format!("{n}.gir"));
            let to = stage.join(format!("{n}.gir"));
            if !to.exists() {
                std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {}: {e}", from.display()));
            }
        }
        std::fs::copy(&gir, stage.join("GtkMcp-1.gir")).unwrap();
        run(
            Command::new("flatpak")
                .args(["run", "--command=g-ir-compiler"])
                .arg(format!("--filesystem={}", out.display()))
                .arg("org.freedesktop.Sdk//25.08")
                .arg(format!("--includedir={}", stage.display()))
                .arg("--shared-library=libgtk_mcp_agent_capi.so")
                .arg(stage.join("GtkMcp-1.gir"))
                .arg("-o")
                .arg(&typelib),
            "flatpak run g-ir-compiler (org.freedesktop.Sdk//25.08; install it or gobject-introspection)",
        );
    }
    assert!(typelib.metadata().is_ok_and(|m| m.len() > 0), "typelib was not produced");
}

fn artifacts() -> &'static Artifacts {
    static A: OnceLock<Artifacts> = OnceLock::new();
    A.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args(["build", "-p", "gtk-mcp-agent-capi"]);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        cmd.env("CARGO_TARGET_DIR", profile_dir.parent().unwrap());
        run(&mut cmd, "cargo build -p gtk-mcp-agent-capi");
        let lib = profile_dir.join("libgtk_mcp_agent_capi.so");
        assert!(lib.exists(), "cdylib not found at {}", lib.display());
        let out = profile_dir.join("m7");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(&lib, out.join("libgtk_mcp_agent_capi.so")).unwrap();
        compile_typelib(&out);
        compile_c(&crate_dir().join("smoke/smoke.c"), &out.join("smoke-c"), &out);
        Artifacts { lib: out.join("libgtk_mcp_agent_capi.so"), out }
    })
}

fn pkg_config(args: &[&str]) -> Vec<String> {
    let o = run(Command::new("pkg-config").args(args).arg("gtk4"), "pkg-config gtk4");
    o.split_whitespace().map(String::from).collect()
}

/// Compile a C program against the header and link the cdylib.
fn compile_c(src: &Path, dst: &Path, out: &Path) {
    let mut cmd = Command::new("cc");
    cmd.arg("-Wall")
        .arg("-Werror")
        .arg(src)
        .arg(format!("-I{}", crate_dir().join("include").display()))
        .args(pkg_config(&["--cflags", "--libs"]))
        .arg(format!("-L{}", out.display()))
        .arg("-lgtk_mcp_agent_capi")
        .arg(format!("-Wl,-rpath,{}", out.display()))
        .arg("-o")
        .arg(dst);
    run(&mut cmd, &format!("cc {}", src.display()));
}

fn env_for(a: &Artifacts) -> Vec<(&'static str, String)> {
    vec![
        ("GI_TYPELIB_PATH", a.out.display().to_string()),
        ("LD_LIBRARY_PATH", a.out.display().to_string()),
    ]
}

// ---- header / exports / linking (no display needed) --------------------------------

fn words_after(text: &str, prefix: &str) -> BTreeSet<String> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(prefix)
        .filter(|(i, _)| !text[..*i].ends_with(ident))
        .map(|(i, _)| {
            let tail = &text[i..];
            tail[..tail.find(|c: char| !ident(c)).unwrap_or(tail.len())].to_string()
        })
        .collect()
}

#[test]
fn header_gir_and_exports_agree() {
    let a = artifacts();
    let header = std::fs::read_to_string(crate_dir().join("include/gtk-mcp-agent.h")).unwrap();
    let declared: BTreeSet<String> = words_after(&header, "gtk_mcp_agent_");
    let gir = std::fs::read_to_string(crate_dir().join("GtkMcp-1.gir")).unwrap();
    let in_gir: BTreeSet<String> = words_after(&gir, "gtk_mcp_agent_");
    let nm = run(Command::new("nm").arg("-D").arg("--defined-only").arg(&a.lib), "nm -D");
    let exported: BTreeSet<String> = nm
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .filter(|s| s.starts_with("gtk_mcp_agent_"))
        .map(String::from)
        .collect();
    assert!(declared.len() >= 5, "{declared:?}");
    assert_eq!(declared, exported, "header declarations and cdylib exports differ");
    assert_eq!(declared, in_gir, "header declarations and GtkMcp-1.gir c:identifiers differ");
}

#[test]
fn cdylib_links_host_gtk_dynamically_and_never_adwaita() {
    let a = artifacts();
    let ldd = run(Command::new("ldd").arg(&a.lib), "ldd");
    assert!(ldd.contains("libgtk-4.so") && !ldd.contains("not found"), "{ldd}");
    for banned in ["adwaita", "layer-shell"] {
        assert!(!ldd.contains(banned), "cdylib links {banned}:\n{ldd}");
    }
    // No embedded GTK: gtk symbols stay undefined in the cdylib.
    let defined = run(Command::new("nm").arg("-D").arg("--defined-only").arg(&a.lib), "nm");
    assert!(!defined.lines().any(|l| l.contains(" gtk_application_")), "the cdylib defines GTK symbols");
    let undef = run(Command::new("nm").arg("-D").arg("--undefined-only").arg(&a.lib), "nm");
    assert!(undef.contains("gtk_is_initialized"), "expected an undefined reference to the host's gtk_is_initialized");
}

#[test]
fn c_program_compiles_against_header_and_is_inert_without_env() {
    let a = artifacts();
    let src = a.out.join("inert.c");
    std::fs::write(
        &src,
        r#"#define _GNU_SOURCE
#include <gtk-mcp-agent.h>
#include <dlfcn.h>
#include <link.h>
#include <string.h>
#include <stdio.h>

static int destroyed;
static char *state (gpointer d) { return g_strdup ("{}"); }
static void gone (gpointer d) { destroyed++; }
static int count_gtk (struct dl_phdr_info *i, size_t s, void *n) {
  if (strstr (i->dlpi_name, "libgtk-4")) (*(int *) n)++;
  return 0;
}

int main (void) {
  int gtk_objs = 0;
  /* the host's GTK is already loaded (we link it); the cdylib must reuse it */
  dl_iterate_phdr (count_gtk, &gtk_objs);
  if (gtk_objs != 1) { printf ("libgtk-4 loaded %d times\n", gtk_objs); return 2; }
  void *self = dlopen (NULL, RTLD_NOW);
  if (dlsym (self, "gtk_mcp_agent_attach") == NULL) return 3;
  /* inert: GTK_MCP is unset, nothing may happen and no GTK init is needed */
  gtk_mcp_agent_attach (NULL);
  gtk_mcp_agent_mark_secret (NULL);
  gtk_mcp_agent_register_action_group (NULL, "x", NULL);
  gtk_mcp_agent_register_settings (NULL);
  gtk_mcp_agent_register_state ("s", state, NULL, gone);
  if (destroyed != 1) { printf ("destroy notify not run while inert (%d)\n", destroyed); return 4; }
  gtk_mcp_agent_register_state ("s", NULL, NULL, gone);
  if (destroyed != 2) return 5;
  puts ("ok");
  return 0;
}
"#,
    )
    .unwrap();
    let exe = a.out.join("inert");
    compile_c(&src, &exe, &a.out);
    let mut cmd = Command::new(&exe);
    cmd.env_remove("GTK_MCP");
    assert_eq!(run(&mut cmd, "inert C program").trim(), "ok");
}

// ---- typelib import (no display needed) --------------------------------------------

#[test]
fn typelib_imports_from_python_and_gjs() {
    let a = artifacts();
    let py = "import gi\ngi.require_version('GtkMcp','1')\nfrom gi.repository import GtkMcp\n\
              assert all(callable(getattr(GtkMcp, n)) for n in ('attach','mark_secret','register_action_group','register_settings','register_state'))\nprint('py-ok')";
    let mut cmd = Command::new("python3");
    cmd.args(["-c", py]).envs(env_for(a));
    assert!(run(&mut cmd, "python import GtkMcp").contains("py-ok"));

    let js = a.out.join("import.js");
    std::fs::write(
        &js,
        "import GtkMcp from 'gi://GtkMcp';\nfor (const n of ['attach','mark_secret','register_action_group','register_settings','register_state'])\n  if (typeof GtkMcp[n] !== 'function') throw new Error('missing ' + n);\nprint('js-ok');\n",
    )
    .unwrap();
    let mut cmd = Command::new("gjs");
    cmd.args(["-m"]).arg(&js).envs(env_for(a));
    assert!(run(&mut cmd, "gjs import GtkMcp").contains("js-ok"));
}

// ---- smoke apps through the real broker --------------------------------------------

fn spawn_app(display: &Display, a: &Artifacts, mut cmd: Command) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    cmd.env("GTK_MCP", "1")
        .env("GTK_MCP_DIR", dir.path())
        .env("XDG_RUNTIME_DIR", &display.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &display.wayland_display)
        .env("GDK_BACKEND", "wayland")
        .env("GSETTINGS_BACKEND", "memory")
        .env("GTK_A11Y", "none")
        .envs(env_for(a))
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(dir.path().join("fixture.stderr")).unwrap()));
    // SAFETY: only async-signal-safe prctl between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn smoke app");
    let pid = child.id();
    let mut f = Fixture { child, dir, pid };
    let t = Instant::now();
    while !f.socket().exists() {
        assert!(t.elapsed() < SMOKE_TIMEOUT, "agent socket did not appear in {SMOKE_TIMEOUT:?}; stderr:\n{}", f.stderr());
        if let Ok(Some(st)) = f.child.try_wait() {
            panic!("smoke app exited early ({st}); stderr:\n{}", f.stderr());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f
}

async fn check_app(fx: &Fixture, lang: &str, token: &str) {
    let policy = Policy { toolsets: vec!["core".into(), "actions".into(), "app".into()], ..Policy::default() };
    let broker: std::sync::Arc<Broker> = Broker::new(policy, Config::with_socket_dir(fx.dir.path())).unwrap();
    let c = Client { broker: broker.clone() };

    let apps = c.json("list_apps", json!({})).await;
    let apps = apps.as_array().unwrap();
    assert_eq!(apps.len(), 1, "{apps:?}\nstderr:\n{}", fx.stderr());
    assert_eq!(apps[0]["pid"], fx.pid);
    assert_eq!(apps[0]["state"], "running", "{}", apps[0]);
    assert_eq!(apps[0]["hello"]["gtk_version"][0], 4);
    assert!(apps[0]["hello"]["adw_version"].is_null(), "no libadwaita in the {lang} app");

    // the window appears asynchronously after activate
    let t = Instant::now();
    let snap = loop {
        let s = c.snapshot_json(json!({})).await;
        if find_node(&s, &|n| n["identity"]["automation_id"] == "smoke-label").is_some() {
            break s;
        }
        assert!(t.elapsed() < SMOKE_TIMEOUT, "{lang}: no smoke-label in snapshot after {SMOKE_TIMEOUT:?}: {s}\nstderr:\n{}", fx.stderr());
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let label = node_by_name(&snap, "smoke-label");
    assert_eq!(label["type"], "GtkLabel");
    assert!(label.to_string().contains(&format!("hello from {lang}")), "{label}");

    // mark_secret reached the agent
    let secret = node_by_name(&snap, "smoke-secret");
    assert_eq!(secret["secret"], true, "{secret}");
    assert!(!snap.to_string().contains(token), "secret text leaked into the snapshot");
    let text = c.call("snapshot", json!({})).await.unwrap().text_content().to_string();
    assert!(!text.contains(token));

    // register_action_group reached the agent
    let acts = c.json("list_actions", json!({"scope": {"automation_id": "smoke-panel"}, "prefix": "smoke"})).await;
    assert!(acts.to_string().contains("smoke.ping"), "{acts}");

    // register_state: callback ran in the app, JSON parsed by the agent
    let list = broker.list_state_resources().await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].name, "smoke");
    let v = broker.read_state_resource(&list[0].uri).await.unwrap();
    assert_eq!(v["lang"], lang);
    assert_eq!(v["tag"], "one");
    broker.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn c_app_attaches_and_serves_snapshot() {
    let display = require_display!();
    let a = artifacts();
    let fx = spawn_app(&display, a, Command::new(a.out.join("smoke-c")));
    check_app(&fx, "c", "c-hunter2-TOKEN-0xC0FFEE").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn python_app_attaches_and_serves_snapshot() {
    let display = require_display!();
    let a = artifacts();
    let mut cmd = Command::new("python3");
    cmd.arg(crate_dir().join("smoke/smoke.py"));
    let fx = spawn_app(&display, a, cmd);
    check_app(&fx, "python", "py-hunter2-TOKEN-0xC0FFEE").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gjs_app_attaches_and_serves_snapshot() {
    let display = require_display!();
    let a = artifacts();
    let mut cmd = Command::new("gjs");
    cmd.arg("-m").arg(crate_dir().join("smoke/smoke.js"));
    let fx = spawn_app(&display, a, cmd);
    check_app(&fx, "gjs", "js-hunter2-TOKEN-0xC0FFEE").await;
}
