<p align="center">
  <img src="assets/icon.svg" width="96" height="96" alt="gtk-mcp icon: a window of nested widgets under a magnifying glass">
</p>

# gtk-mcp

[![QA](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/qa.yml/badge.svg)](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/qa.yml)
[![Release](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

gtk-mcp lets a coding agent inspect and drive a running GTK4 or libadwaita app from inside the app's own process. The agent can read the widget tree, look at layout and styles, press buttons and fill in entries by calling GTK directly, take pictures of individual widgets, and watch what changes. It does not simulate keyboard or mouse input, and it does not take screenshots of your desktop. Use wayland-mcp for that kind of testing.

The project has two parts. The broker is the MCP server your client talks to over stdio. The agent is a small library you add to your app during development. The two talk over a Unix socket in your runtime directory, `$XDG_RUNTIME_DIR/gtk-mcp` by default. Set `GTK_MCP_DIR` for both the app and the broker if you want the socket somewhere else.

## What it can do

The broker has 56 tools in six toolsets: `core`, `actions`, `layout-style`, `diagnostics`, `a11y` and `app`. You choose which toolsets to turn on when you start the broker.

With `core`, the agent lists the apps and windows it can see, prints the widget tree as text or JSON, finds widgets by type, label, CSS class or the name you gave them, and reads any GObject property. It can also activate buttons, open expanders, switch tabs, fill in entries, edit text buffers, and wait until the interface reaches a state you describe. A call that changes something reports the state it found afterwards, so a slider that clamped your value or an entry that rejected your text doesn't come back as a success.

`actions` covers GActions, menu models and shortcuts. `layout-style` measures widgets, flags labels that got ellipsized and children that overflow their parent, and applies temporary CSS, so you can try a style and see its parse errors without rebuilding. `diagnostics` watches property changes, reads the app's logs, measures frame times, and records a session of you using the app as a script the agent can replay. `a11y` reads the accessible tree and checks it against a set of rules. `app` opens files through GApplication, reads and writes GSettings, and exposes state your app registers itself.

GTK renders each picture from the widget itself instead of reading it off the screen. That's why a capture works on a covered window, and on layer-shell surfaces like panels, where a desktop screenshot would need the compositor's help.

## Install

You need Rust and GTK 4.10 or newer. Build the broker with:

```sh
cargo build --release -p gtk-mcp
```

The binary ends up in `target/release/gtk-mcp`. Add it to your MCP client as a stdio server:

```json
{
  "mcpServers": {
    "gtk-mcp": {
      "command": "/path/to/gtk-mcp",
      "args": ["--toolsets", "core,app"]
    }
  }
}
```

## Add it to your app

Add the agent as an optional dependency behind a Cargo feature, and leave the feature off in release builds:

```toml
[features]
gtk-mcp = ["dep:gtk-mcp-agent"]

[dependencies]
gtk-mcp-agent = { version = "0.1", optional = true }
```

Then call `attach` once, on the main thread, after your application object exists. With Relm4 that's right after `RelmApp::new`:

```rust
#[cfg(all(debug_assertions, feature = "gtk-mcp"))]
gtk_mcp_agent::attach(&relm4::main_application());
```

That `#[cfg]` mark is the only gate: calling `attach` activates the agent (socket on startup, mutations accepted), and not calling it — feature off, or the mark changed to `#[cfg(all(debug_assertions, feature = "gtk-mcp"))]` — leaves the app untouched. There is no runtime environment gate. Removing the `debug_assertions` condition from the mark enables the agent in release builds too.

Give widgets stable names where you can. After `entry.set_widget_name("server-address")`, the agent finds that entry as `server-address` whatever its label says and whatever language the UI is in.

Three optional calls cover the rest. `register_state("panels", ...)` gives the broker a closure that returns your app state as JSON, and that state shows up as a read-only MCP resource. `register_settings(&settings)` tells the GSettings tools which settings object your app uses. `mark_secret(&widget)` keeps a widget's text out of every response. You don't need it for password entries, because the agent detects those by itself.

C, Python and GJS apps use the C library in `crates/gtk-mcp-agent-capi` instead. It ships a header and a GObject Introspection typelib, and you use it the same way: call `GtkMcp.attach(app)` before you run the application.

## Use it

Start your app (built with the feature and the `attach` call compiled in):

```sh
./your-app
```

Then ask your coding agent to take a look. In a typical round, `list_apps` finds your process, `snapshot` prints the window, `query` picks out one widget, `interact` opens the expander and fills the entry, `wait_for` waits until the Save button becomes sensitive, and `capture` renders the expander so the agent can see the result. After a rebuild, `restart_app` (with `--allow-launch`) starts the new binary, and the same session carries on against it.

`--toolsets` chooses which tools the client sees and defaults to `core`. Add `app` if you registered state, and `a11y` if you want accessibility checks. The `a11y` tools also need `--allow-atspi` and an app started with `GTK_A11Y=atspi`. `--allow-launch` lets the broker start and stop apps itself, and comes with presets for things like in-process file choosers and translation testing.

## Security

The agent is present only where the developer compiled the `attach` call in; a build without it has no agent at all. The socket is readable only by your user. Before the broker talks to a peer, it checks that the peer runs as your user and is the process the socket is named after. Password fields and anything you mark secret never appear in responses, not even as a length, and a capture refuses a password field while the field is showing its text. Other sensitive pixels can still end up in a capture, so don't enable the agent in a session with real credentials on screen.

## More

`docs/tools.md` lists every tool. `docs/embedding.md` covers the embedding API in detail, `docs/widget-tree.md` describes the snapshot format, and `docs/plan.md` is the design document the implementation was built from.
