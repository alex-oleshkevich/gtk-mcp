# gtk-mcp: driving a running GTK4 app from the shell

gtk-mcp inspects and operates a running GTK4/libadwaita app from inside its process: widget tree,
properties, layout, styles, actions, accessibility, and PNG captures rendered by GTK itself.
It does not synthesize keyboard/mouse input or take desktop screenshots.

Only apps built with the gtk-mcp agent compiled in (`gtk_mcp_agent::attach`, or `GtkMcp.attach` from
C/Python/GJS) are visible. If `gtk-mcp apps` shows nothing, start the app first; if it still shows
nothing, run `gtk-mcp doctor`. Both sides must agree on `GTK_MCP_DIR` when it is set.

## Commands

```sh
gtk-mcp apps                         # running apps: session, pid, app id, GTK version, state
gtk-mcp state [NAME] [--app APP] [--json] # list registered app states, or read NAME as JSON
gtk-mcp tools                        # every tool by toolset; R reads, M changes something
gtk-mcp tools NAME                   # one tool: description and ARGS_JSON schema (--json for JSON)
gtk-mcp call TOOL ['ARGS_JSON'] [--app APP] [-o FILE.png]
echo 'ARGS_JSON' | gtk-mcp call TOOL - # arguments from stdin; use this for long or quoted JSON
gtk-mcp doctor                       # socket directory, app sockets, AT-SPI bus
```

- Results go to stdout as pretty JSON (text snapshots print as plain text). Errors go to stderr as
  JSON `{code, message, data?}` and exit 1. Read `code`: `ambiguous_target` lists candidates,
  `stale_reference` means the widget is gone, `detached` means it is not inside a window.
  Calls also print `gtk-mcp: progress …` lines to stderr as updates arrive; stdout stays result-only.
- `--app` takes a pid, an app id or a session id. Leave it out when only one app runs. Prefer pid
  or app id: session ids (`s1`, ...) are assigned per command and can differ between commands.
- `-o FILE.png` saves the PNG of `capture` (also inside `batch`; extra images go to `FILE-2.png`, ...).
  Without `-o` images are dropped. Look at the saved file to see the result.
- When unsure of a tool's arguments, run `gtk-mcp tools NAME` before guessing.
- `state` reads JSON shared by the app with `register_state`, not widget properties. Names are
  relative to `app-state/` (`app-state/panels` is read as `state panels`). Without a name, it lists
  session, app id and name (`--json` for JSON). `--app` filters the list or picks the app to read;
  otherwise a read selects the only app with that name, or lists candidates if several have it.

## Targets

Most tools take `target`: a ref string or a selector object.

- Refs: `w12` for widgets, `o4` for other objects, printed in snapshots and results. A ref stays
  valid across commands for as long as the app process lives and the object exists. A restarted
  app has new refs.
- Selector: all keys must match. `{"automation_id": "server-address"}` (the widget name),
  `{"type_name": "GtkButton", "text": "Save"}`, `{"css_class": "suggested-action"}`,
  `{"text_contains": "Advanced"}`, `{"role": "button"}`, `{"state": {"sensitive": true}}`,
  `{"ancestor": {...}}`, `{"window": {"title": "Preferences"}}`, `{"nth": 0}`.
  Single-target tools need exactly one match; otherwise add keys or `nth`.
- Secret widgets (password fields, anything marked secret) never expose their text and never
  match `text`/`text_contains`.

## Loop

1. Look: `gtk-mcp call snapshot '{"view":"semantic","format":"text"}'`. Use `max_depth`/`max_nodes`
   for big windows and `target` for a subtree. `list_windows` shows popovers and other surfaces.
2. Find: `gtk-mcp call query '{"selector":{"type_name":"GtkButton","text":"Save"}}'`, then
   `inspect` (type, state, capabilities: which `interact` operations work), `get_property`
   (`{"target":"w18","names":["label","sensitive"]}`), `inspect_layout`, `inspect_style`.
3. Act: `gtk-mcp call interact '{"target":"w10","operation":"set_expanded","expanded":true}'`.
   Operations: activate, focus, fill {text}, set_checked {checked}, set_active {active},
   set_value {value}, set_expanded {expanded}, select {index}, select_page {name}, popup, popdown,
   scroll_to, scroll_by {dx,dy}, close, play, pause, seek {microseconds}. Mutating tools read the
   result back (`observed`); trust that over your expectation. Add `"observe":{"snapshot_depth":2}`
   to get the subtree after the change. Also: `set_property`, `edit_text`, `activate_action`,
   `activate_shortcut` (e.g. `<Control>s`), `app_open`, `set_gsetting`.
4. Wait: `gtk-mcp call wait_for '{"target":{"automation_id":"status"},"condition":"text","contains":"Saved"}'`.
   Conditions: exists, absent, match_count, property, text, mapped, sensitive, app_busy. A timeout
   returns `satisfied:false` (default 5000 ms, `timeout_ms` up to 60000); it is not an error.
5. See: `gtk-mcp call capture '{"target":"w10"}' -o /tmp/w10.png`, then view the PNG. Captures
   refuse unmapped targets and secret fields showing text.

Checks without acting: `audit_layout` (ellipsized labels, overflow, overlaps, missing icons),
`audit_accessibility` and `accessible_tree` (start the app with `GTK_A11Y=atspi` and pass
`--allow-atspi` for AT-SPI data), `inspect_shortcuts`, `list_actions`, `inspect_menu`,
`inspect_model` (all rows of list views, not only the realized ones).

## Each `call` is its own connection

Every `gtk-mcp call` starts a fresh broker, connects, runs one tool and disconnects. State that
belongs to a connection is gone when the command exits:

- `apply_css`, `highlight` and `override_setting` are reverted at exit; `remove_css` handles do not
  carry over.
- `watch` subscriptions, `read_events` cursors and `record`/`stop_recording` do not carry over.
- `snapshot_id`/`capture_id` are not retained, so `diff_snapshots`/`diff_captures` cannot compare
  results of two commands. Compare the outputs yourself.
- Apps started with `call launch_app --allow-launch` stop when the command exits. Start the app
  yourself in the background instead.

Registered app state is also available from the shell with `gtk-mcp state`; unlike the connection
state above, it belongs to the app and remains available across commands.

Use `batch` when steps must share a connection: one call, steps run in order on the app's main
loop, `$N.ref` is the target of step N (numbered from 1), and `capture` steps honour `-o`.

```sh
gtk-mcp call batch - -o /tmp/after.png <<'EOF'
{"steps":[
  {"tool":"apply_css","arguments":{"css":"#status { background: red; }"}},
  {"tool":"interact","arguments":{"target":{"automation_id":"save"},"operation":"activate"}},
  {"tool":"get_property","arguments":{"target":"$2.ref","names":["sensitive"]}},
  {"tool":"capture","arguments":{"target":{"automation_id":"status"}}}
]}
EOF
```

`batch` stops at the first error unless `"stop_on_error": false`, takes at most 100 steps, and
cannot contain `batch`, lifecycle tools or the diff tools. Nothing is rolled back.

To validate CSS without rebuilding, run `apply_css` alone: it returns parse errors with line and
column, and is reverted when the command exits.

After rebuilding the app, restart it yourself and re-read refs with `snapshot` or `query`.
