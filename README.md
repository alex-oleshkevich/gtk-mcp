# gtk-mcp

gtk-mcp lets a coding agent work with a running GTK4 or libadwaita app from inside the app's own process. The agent can read the widget tree, look at layout and styles, press buttons and fill in entries by calling GTK directly, take pictures of individual widgets, and watch what changes. It never simulates keyboard or mouse input, and it never takes screenshots of your desktop. That kind of testing belongs to wayland-mcp.

Two programs do the work. The broker is the MCP server your client talks to over stdio. The agent is a small library you add to your app during development. They talk over a Unix socket in your runtime directory.

## What it can do

There are 56 tools, grouped into toolsets you turn on when you start the broker.

With the core toolset, the agent lists the apps and windows it can see, prints the widget tree as text or JSON, finds widgets by type, label, CSS class or the name you gave them, and reads any GObject property. It can activate buttons, open expanders, switch tabs, fill in entries, edit text buffers and wait for the interface to reach a state you describe. Calls that change something report the state they actually found afterwards, so a clamped slider or an entry that rejected your text does not get reported as success.

The actions toolset covers GActions, menu models and shortcuts. The layout-style toolset measures widgets, flags labels that got ellipsized and children that overflow their parent, and applies temporary CSS so you can try a style and see the parse errors without rebuilding. The diagnostics toolset watches property changes, reads the app's logs, measures frame times and records a session of you using the app as a script the agent can replay. The a11y toolset reads the accessible tree and checks it against a set of rules. The app toolset opens files through GApplication, reads and writes GSettings, and exposes state your app registers itself.

Pictures come from GTK rendering the widget, not from the screen. A capture works on a covered window and on layer-shell surfaces like panels, where a desktop screenshot would need the compositor's help.

## Install

You need Rust and GTK 4.10 or newer on the machine. Build the broker with:

```sh
cargo build --release -p gtk-mcp
```

The binary lands in `target/release/gtk-mcp`. Point your MCP client at it as a stdio server:

```json
{
  "mcpServers": {
    "gtk-mcp": {
      "command": "/path/to/gtk-mcp",
      "args": ["--toolsets", "core,app", "--allow-mutations"]
    }
  }
}
```

## Add it to your app

Add the agent as an optional dependency, behind a Cargo feature you leave off in release builds:

```toml
[features]
gtk-mcp = ["dep:gtk-mcp-agent"]

[dependencies]
gtk-mcp-agent = { version = "0.1", optional = true }
```

Then call `attach` once, on the main thread, after your application object exists. With Relm4 that is right after `RelmApp::new`:

```rust
#[cfg(feature = "gtk-mcp")]
gtk_mcp_agent::attach(&relm4::main_application());
```

Nothing happens unless `GTK_MCP=1` is in the environment when the app starts, so a debug build is safe to leave on your machine. Mutations need one more variable, `GTK_MCP_MUTATIONS=1`, and the broker's `--allow-mutations` flag. Both sides have to agree.

Give widgets stable names where you can. `entry.set_widget_name("server-address")` makes the widget findable as `server-address` no matter what its label says or which language the UI is in.

Three optional calls cover the rest. `register_state("panels", ...)` hands the broker a closure that returns your app state as JSON, which shows up as a read-only MCP resource. `register_settings(&settings)` tells the GSettings tools which settings object your app uses. `mark_secret(&widget)` keeps a widget's text out of every response. Password entries are detected on their own.

C, Python and GJS apps use the C library in `crates/gtk-mcp-agent-capi` instead. It ships a header and a GObject Introspection typelib, and the call is the same idea: `GtkMcp.attach(app)` before you run the application.

## Use it

Start your app with the agent on:

```sh
GTK_MCP=1 GTK_MCP_MUTATIONS=1 ./your-app
```

Then ask the coding agent to look. A typical round goes like this: `list_apps` finds your process, `snapshot` prints the window, `query` picks out one widget, `interact` opens the expander and fills the entry, `wait_for` waits until the Save button becomes sensitive, and `capture` renders the expander so the agent can see the result. After a rebuild, `restart_app` (with `--allow-launch`) starts the new binary and the same session continues against it.

Two flags are worth knowing early. `--toolsets` chooses which tools the client sees, and defaults to `core`; add `app` if you registered state, `a11y` if you want accessibility checks (that also needs `--allow-atspi` and the app started with `GTK_A11Y=atspi`). `--allow-launch` lets the broker start and stop apps itself, with presets for things like in-process file choosers and translation testing.

## Security

The agent stays off without `GTK_MCP=1`. The socket is readable only by your user, and the broker checks that the peer runs as your user and is the process the socket is named after before it talks. Password fields and anything you mark secret never appear in responses, not even their length, and captures refuse a password field while it is showing its text. Captures can still show other sensitive pixels, so do not enable the agent on a session with real credentials on screen.

## More

`docs/tools.md` lists every tool. `docs/embedding.md` covers the embedding API in detail, and `docs/widget-tree.md` describes the snapshot format. `docs/plan.md` is the design document the implementation was built from.
