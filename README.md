<p align="center">
  <img src="assets/icon.svg" width="96" height="96" alt="gtk-mcp icon: a window of nested widgets under a magnifying glass">
</p>

# gtk-mcp

[![QA](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/qa.yml/badge.svg)](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/qa.yml)
[![Release](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/alex-oleshkevich/gtk-mcp/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

gtk-mcp lets a coding agent look inside a running GTK4 or libadwaita app and use it. It works from inside the app's own process. The agent can read the widget tree, check layout and styles, press buttons and fill in text fields, take pictures of single widgets, and watch for changes. It does all of this by calling GTK directly. It does not fake keyboard or mouse input, and it does not take screenshots of your desktop. Use wayland-mcp for that.

There are two parts. The broker is the MCP server: your MCP client starts it and talks to it over stdio. The agent is a small library you add to your app while you develop it. The broker and the agent talk over a Unix socket. By default the socket lives in `$XDG_RUNTIME_DIR/gtk-mcp`. To put it somewhere else, set `GTK_MCP_DIR` for both the app and the broker.

## What it can do

The broker has 56 tools in six groups, called toolsets: `core`, `actions`, `layout-style`, `diagnostics`, `a11y` and `app`. You pick which toolsets to turn on when you start the broker.

`core` is the basic set. With it, the agent can:

- list the apps and windows it can see;
- print the widget tree as text or JSON;
- find widgets by type, label, CSS class, or a name you gave them;
- read any GObject property;
- click buttons, open expanders, switch tabs, fill in entries, and edit text;
- wait until the window reaches a state you describe.

Every tool that changes something also reads the result back. If a slider clamps your value or an entry rejects your text, the agent sees that instead of a false success.

The other toolsets:

- `actions`: the app's actions (GAction), menus and keyboard shortcuts.
- `layout-style`: widget sizes, labels cut off with "…", children that spill out of their parent, and temporary CSS. You can try a style and see its errors without rebuilding.
- `diagnostics`: property change watches, the app's logs, and frame times. It can also record you using the app and turn that into a script the agent can replay.
- `a11y`: the accessibility tree, plus checks against a set of rules.
- `app`: opening files through GApplication, reading and writing GSettings, and app state that your app chooses to share.

Widget pictures come from GTK itself, not from the screen. So a capture works even when the window is covered, and on panels and other layer-shell surfaces, where a desktop screenshot would need help from the compositor.

## Install

Download a prebuilt broker for Linux or macOS (x86_64 or arm64) from the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/alex-oleshkevich/gtk-mcp/main/install.sh | sh
```

This puts `gtk-mcp` in `~/.local/bin`. To pick a version or another directory, set `GTK_MCP_VERSION=v0.1.0` or `BIN_DIR=/some/dir` before `sh`.

To build it yourself instead, you need Rust and GTK 4.10 or newer:

```sh
cargo build --release -p gtk-mcp
```

The binary is then `target/release/gtk-mcp`. Either way, add it to your MCP client as a stdio server:

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

Add the agent as an optional dependency behind a Cargo feature. Leave the feature off in release builds:

```toml
[features]
gtk-mcp = ["dep:gtk-mcp-agent"]

[dependencies]
gtk-mcp-agent = { version = "0.2", optional = true }
```

Then call `attach` once, on the main thread, after your application object exists. With Relm4, do it right after `RelmApp::new`:

```rust
#[cfg(all(debug_assertions, feature = "gtk-mcp"))]
gtk_mcp_agent::attach(&relm4::main_application());
```

That `#[cfg]` line is the only switch. With it, `attach` is compiled only into debug builds that have the feature turned on. If `attach` is compiled in, the agent runs: it opens its socket when the app starts and accepts changes. If it is not compiled in, the app has no agent at all. No environment variable turns the agent on or off. If you remove `debug_assertions` from the line, release builds get the agent too.

Give widgets stable names where you can. After `entry.set_widget_name("server-address")`, the agent can find that entry as `server-address`, whatever its label says and whatever language the app is in.

Three more calls are optional:

- `register_state("panels", ...)` takes a closure that returns your app state as JSON. Read it with `gtk-mcp state panels` or as a read-only MCP resource.
- `register_settings(&settings)` tells the GSettings tools which settings object your app uses.
- `mark_secret(&widget)` keeps a widget's text out of every response. Password fields don't need it, because the agent spots them on its own.

C, Python and GJS apps use the C library in `crates/gtk-mcp-agent-capi`. It comes with a header and a GObject Introspection typelib. You use it the same way: call `GtkMcp.attach(app)` before you run the application.

## Use it

Start your app, built with the feature on and the `attach` call compiled in:

```sh
./your-app
```

Then ask your coding agent to take a look. A typical round goes like this. `list_apps` finds your app. `snapshot` prints the window. `query` picks out one widget. `interact` opens an expander and fills in an entry. `wait_for` waits until the Save button can be clicked. `capture` takes a picture of the expander so the agent can see the result. After you rebuild, `restart_app` starts the new binary and the session carries on. It needs `--allow-launch`.

`--toolsets` picks the tools your client sees. The default is `core`; `all` turns on every toolset. Add `app` if you registered state, and `a11y` for accessibility checks. The `a11y` tools also need `--allow-atspi`, and the app must be started with `GTK_A11Y=atspi`. `--allow-launch` lets the broker start and stop apps itself. It comes with presets, for example for in-app file choosers or testing translations.

## Command line

The same binary has a few commands for the shell. With no command, `gtk-mcp` runs the MCP server, so existing client configs keep working.

```sh
gtk-mcp doctor                                   # checks the socket directory, every app socket and the AT-SPI bus
gtk-mcp apps                                     # lists running apps: session, pid, app id, GTK version, state
gtk-mcp state                                    # lists registered app states: session, app id, name
gtk-mcp state panels --app org.example.App        # reads one registered state's JSON value
gtk-mcp tools                                    # lists every tool by toolset, R for read-only and M for mutating
gtk-mcp tools interact                           # one tool's description and the JSON schema of its arguments
gtk-mcp call capture '{"target":"w1"}' -o a.png  # runs one tool and saves the picture
gtk-mcp prime                                    # prints a usage guide for coding agents
```

`doctor` only reads. It exits 1 when the socket directory is unusable or an app fails its handshake. `call` takes the tool name, the arguments as JSON (or `-` to read them from stdin) and `--app` with a session id, a pid or an app id. You can leave `--app` out when only one app runs. Results are printed as JSON, errors go to stderr and exit with 1. Progress updates go to stderr as `gtk-mcp: progress …`; stdout contains only the result. Every toolset is on for `call`. `--allow-launch` and `--allow-atspi` work as they do for the server. `--json` on `doctor`, `apps`, `tools` and `state` gives machine output.

`state` lists the JSON states registered by running apps with `register_state`. With a name, it prints that state's value as pretty JSON. Names are relative to `app-state/`: a registration named `app-state/panels` is read with `gtk-mcp state panels`. `--app` accepts the same selectors as `call` and also filters the list. Without `--app`, a read selects the only app with that name; if several apps have it, the error lists candidates. An unknown name reports the available names.

Each `call` opens its own connection to the app and closes it on exit. Refs such as `w12` stay valid between calls, but anything tied to the connection does not: temporary CSS, highlights and setting overrides are reverted, watches and recordings end, and snapshot and capture ids can't be diffed across calls. Put steps that need to share that state in one `batch`.

### As an agent skill

Coding agents that support skills can use the command line instead of the MCP server. Install the skill from `skills/gtk-mcp`:

```sh
npx skills add alex-oleshkevich/gtk-mcp
```

or copy `skills/gtk-mcp/SKILL.md` to your agent's skills directory, for example `~/.claude/skills/gtk-mcp/SKILL.md`. The skill tells the agent to run `gtk-mcp prime`, which prints the guide and the tool index from the installed binary, so it always matches your version.

## Security

The agent only exists in builds where you compiled the `attach` call in. The socket is readable only by your user. Before the broker talks to an app, it checks that the app runs as your user and is the process the socket is named after.

Password fields and anything you mark secret never show up in responses, not even their length. A capture refuses a password field while it shows its text. Other private things on screen can still end up in a picture, so don't run the agent while real credentials are visible.

## More

- `docs/tools.md` lists every tool.
- `docs/embedding.md` explains how to add the agent to an app.
- `docs/widget-tree.md` describes the snapshot format.
- `docs/plan.md` is the design document the code was built from.
