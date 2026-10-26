# gtk-mcp

An MCP server that lets a coding agent inspect, exercise, render and diagnose a
running GTK4 / libadwaita application **from inside the application process**.
A broker (`gtk-mcp`, MCP over stdio) talks over a Unix socket to an agent
(`gtk-mcp-agent`) that the app embeds. Real input, desktop screenshots and
compositor state belong to `wayland-mcp`; gtk-mcp never injects input.

Authoritative design: [`docs/plan.md`](docs/plan.md). Tool contracts:
[`docs/tools.md`](docs/tools.md). Embedding details:
[`docs/embedding.md`](docs/embedding.md).

## Install

```sh
cargo build --release -p gtk-mcp          # target/release/gtk-mcp
```

Register it with an MCP client as a stdio server, for example:

```json
{ "command": "/path/to/gtk-mcp", "args": ["--toolsets", "core,app", "--allow-mutations"] }
```

Requirements: GTK 4.10+ in the app (libadwaita and gtk4-layer-shell are never
linked by the agent; they are reached by GType name and `dlsym`).

## Embed the agent in your app

Add `gtk-mcp-agent` behind a Cargo feature that release builds do not enable,
and call `attach` once, on the main thread, after the application object exists:

```toml
[features]
gtk-mcp = ["dep:gtk-mcp-agent"]
[dependencies]
gtk-mcp-agent = { version = "0.1", optional = true }
```

```rust
#[cfg(feature = "gtk-mcp")]
gtk_mcp_agent::attach(&app);
```

The agent is inert unless `GTK_MCP=1` is set at runtime. It binds
`<dir>/<pid>.sock` (directory `0700`, socket `0600`, peers checked with
`SO_PEERCRED`) when the application starts up, and removes `GTK_MCP*` from the
process environment after reading them so apps your app launches are not
instrumented. Optional API (all no-ops when the agent is inactive):

| Function | Purpose |
|---|---|
| `register_state(name, Fn() -> serde_json::Value)` | Expose app state as the MCP resource `gtk-mcp://<session>/app-state/<name>` (needs the `app` toolset). The closure runs on the main thread; re-registering a name replaces it. A leading `app-state/` in `name` is accepted |
| `register_settings(&gio::Settings)` | Tell `inspect_gsettings` / `set_gsetting` which `GSettings` object the app uses |
| `mark_secret(&widget)` | Add a widget to the secret set |

Stable selectors need no dependency: `widget.set_widget_name("panel-clock")`
is the `automation_id`.

## Other languages (C, Python, GJS)

`crates/gtk-mcp-agent-capi` builds `libgtk_mcp_agent_capi.so` (`cargo build -p
gtk-mcp-agent-capi`), with a header (`include/gtk-mcp-agent.h`) and a
GObject-introspection description (`GtkMcp-1.gir`, compiled with
`g-ir-compiler --shared-library=libgtk_mcp_agent_capi.so` to `GtkMcp-1.typelib`).
The library links the host's `libgtk-4` dynamically (no second GTK) and never
links libadwaita or gtk4-layer-shell. Same `GTK_MCP=1` gating and socket
contract as the Rust crate; call everything on the main thread.

```c
#include <gtk-mcp-agent.h>
gtk_mcp_agent_attach (app);              /* before g_application_run() */
```

```python
gi.require_version('GtkMcp', '1'); from gi.repository import GtkMcp
GtkMcp.attach(app)                       # GI_TYPELIB_PATH / LD_LIBRARY_PATH -> build dir
```

```js
import GtkMcp from 'gi://GtkMcp';
GtkMcp.attach(app);
```

Also exported: `mark_secret(GObject)`, `register_action_group(widget, prefix,
group)`, `register_settings(GSettings)` and `register_state(name, callback)`
(the callback returns a JSON string; C signature `char *(*)(gpointer)` plus
`user_data`/`GDestroyNotify`). There is no log-forwarding API (the Rust agent
has none). Working apps: `crates/gtk-mcp-agent-capi/smoke/` (`smoke.c`,
`smoke.py`, `smoke.js`), exercised by `tests/fixture_m7.rs`.

## Policy flags (broker)

| Flag | Meaning |
|---|---|
| `--toolsets core,actions,layout-style,a11y,diagnostics,app` | Tool exposure; default `core`. `--tools` / `--exclude-tools` refine; unknown names fail startup |
| `--allow-mutations` | Mutating tools; the app must also run with `GTK_MCP_MUTATIONS=1` |
| `--allow-launch` | `launch_app`, `restart_app`, `stop_app` (otherwise `launch_disabled`) |
| `--allow-atspi` | AT-SPI source |
| `--retain-snapshots N` (16), `--retain-captures N` (16), `--retain-exited N` (8) | Bounded retention |

## Environment variables

| Name | Where | Meaning |
|---|---|---|
| `GTK_MCP=1` | app | activate the agent |
| `GTK_MCP_MUTATIONS=1` | app | the agent accepts mutations (broker policy also required) |
| `GTK_MCP_DIR` | app and broker | socket directory; default `$XDG_RUNTIME_DIR/gtk-mcp` |

`launch_app` sets `GTK_MCP=1` and `GTK_MCP_DIR` for the child, and
`GTK_MCP_MUTATIONS=1` only when the broker runs with `--allow-mutations`.
Caller-supplied `GTK_MCP*` variables are rejected (`invalid_params`).

## Lifecycle (`launch_app`, `restart_app`, `stop_app`)

- `launch_app {argv, cwd?, env?, presets?, wayland_display?, xdg_runtime_dir?, timeout_ms?}`
  runs `argv` directly (no shell) in its own process group, with stdin closed
  and stdout/stderr captured into a cursor log, then waits (default 15 s) for
  the app's socket and returns the session (`launched_by_broker: true`).
  Presets: `no_portals` (`GDK_DEBUG=no-portals`), `instance_count`
  (`GOBJECT_DEBUG=instance-count`), `a11y` (`GTK_A11Y`), `language`
  (`LANGUAGE`), `gsk_renderer` (`GSK_RENDERER`), `gtk_debug` (`GTK_DEBUG`).
  They are inherited by the app's own children. The display is the broker's,
  or the optional `wayland_display` / `xdg_runtime_dir`; there is no coupling
  to `wayland-mcp`.
- `stop_app {session, timeout_ms?}`: SIGTERM to the process group, SIGKILL after
  `timeout_ms` (default 3000).
- `restart_app {session, timeout_ms?, launch_timeout_ms?}`: stop, then launch the
  same spec again; returns the new session. The old session stays listed as exited.
- `restart_app` / `stop_app` refuse sessions the broker did not launch
  (`unsupported_operation`).
- When an app exits, its session becomes `exited` with `code`/`signal`, the
  in-flight request id, and retained logs/events (`list_apps` shows a
  `post_mortem` object). Library users read the logs with `Session::logs_since`.
  At most `--retain-exited` exited sessions are kept.
- Apps launched by the broker are terminated when the broker exits.

## Tools by toolset in this build

- `core`: `list_apps`, `list_windows`, `snapshot`, `diff_snapshots`, `query`,
  `inspect`, `list_properties`, `get_property`, `set_property`, `list_signals`,
  `interact`, `inspect_text`, `edit_text`, `wait_for`, `batch`, `inspect_model`,
  `capture`, `diff_captures`, `launch_app`, `restart_app`, `stop_app`.
- `app`: `app_open`, `inspect_gsettings`, `set_gsetting`, `list_resources`,
  `read_resource`, plus the `app-state/<name>` MCP resources.
- `diagnostics`: `object_stats` (needs `GOBJECT_DEBUG=instance-count` at app
  start, e.g. the `instance_count` preset, and a GLib built with debug support;
  otherwise `unavailable`).

## Security model

- The agent is off unless `GTK_MCP=1`; mutations need both the broker flag and
  the app's `GTK_MCP_MUTATIONS=1`; launching needs `--allow-launch`.
- Secret widgets (password entries, anything `mark_secret`) never leak text or
  length; GSettings keys whose names look like credentials are reported as
  `{"redacted": true}`. Captures can still contain unmarked sensitive pixels:
  do not enable the agent on sessions with real credentials on screen.
- Every request runs on the GTK main thread; a stuck main loop yields
  `target_unresponsive`, never a hang. Sockets are user-only and peer-checked.
