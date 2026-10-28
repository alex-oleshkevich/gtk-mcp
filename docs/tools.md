# Tools

gtk-mcp works inside the app's process. It covers GTK, GDK, GSK, GIO application state and libadwaita. Real keyboard and mouse input, desktop screenshots, the clipboard, window placement and compositor state are handled by `../wayland-mcp` instead.

## How to read this page

Each tool is marked `R` or `M`:

- `R` tools only read.
- `M` tools change something. They run whenever the tool is listed. The only gate is whether the app compiled the agent in (see `embedding.md`).
- The lifecycle tools (`launch_app`, `restart_app`, `stop_app`) also need the broker flag `--allow-launch`. Add it to the gtk-mcp server's `args` in your MCP client config (for example, the `args` array in `.mcp.json`), then restart the broker.

The MCP tool annotations follow the same split.

The broker lists every tool. The groups below are for reading only. `--tools` offers just the named tools and `--exclude-tools` hides single tools. An unknown name stops the broker from starting.

A target is either a ref (`w12`, `o4`) or a selector (see `plan.md` section 7). Actions are addressed as `{scope, name}`. Tools that change something accept `observe` and return the state after the change. If the change worked but reading the state back failed, you get both results, and nothing is retried.

You can run any tool from the shell with `gtk-mcp call <tool> [ARGS_JSON]`. It uses the same code as the MCP server, with every tool enabled, and prints the result as JSON; progress lines go to stderr as `gtk-mcp: progress …`. Use `gtk-mcp tools` to list the names, `gtk-mcp tools <tool>` for a tool's description and argument schema, and `--app` to pick an app when several run. `gtk-mcp state [NAME] [--app APP] [--json]` lists registered app states or reads one as JSON. Each `call` is its own connection: state that belongs to a connection (temporary CSS, highlights, setting overrides, watches, recordings, retained snapshot and capture ids) ends with the command, and apps started by `call launch_app` stop when it exits. Use `batch` for steps that must share it. `gtk-mcp prime` prints a usage guide for coding agents.

## core

| Tool | | Purpose |
|---|---|---|
| `list_apps` | R | Sessions: PID, app ID, GTK/libadwaita versions, capabilities, state (`running`, `incompatible`, `exited` with code/signal, in-flight request, retained logs/events), `launched_by_broker` |
| `list_windows` | R | Toplevels and mapped native surfaces (popovers): title, visibility, size, refs, renderer, surface scale, monitor connector, modal / transient-for / default widget, layer-shell configuration when gtk4-layer-shell is loaded |
| `launch_app` | M | Start an app from an argv list (no shell) with GTK presets (`GDK_DEBUG=no-portals`, `GOBJECT_DEBUG=instance-count`, `GTK_A11Y`, `LANGUAGE`, `GSK_RENDERER`, `GTK_DEBUG`) and an optional `WAYLAND_DISPLAY`. Captures stdout/stderr. Your environment passes through; the broker sets `GTK_MCP_DIR` to its socket directory. The app's own child processes inherit the presets, and `no-portals` also turns off portal OpenURI |
| `restart_app` | M | Only for sessions the broker launched. Starts a new session; old refs stop working |
| `stop_app` | M | Only for sessions the broker launched. Sends SIGTERM, then SIGKILL after a timeout |
| `snapshot` | R | Widget tree, `full` or `semantic`, `text` or `json` (`widget-tree.md`). Optional `root` (alias `target`) selects a ref or selector; omitted: all toplevels. Unknown parameter keys are rejected. Returns `snapshot_id` |
| `diff_snapshots` | R | Compare two kept snapshots by `snapshot_id`. Changes are listed per widget ref |
| `query` | R | Find all refs that match a selector |
| `inspect` | R | Type, ancestors, identities, state (`live` / `detached`), capabilities, static tooltip, related objects as refs |
| `list_properties` | R | Property definitions, filtered by owner class or name |
| `get_property` | R | Read values. Object values come back as refs; secret objects are hidden |
| `set_property` | M | Write a property and return the value read back |
| `list_signals` | R | Signal definitions, and whether each can be watched |
| `interact` | M | Run one typed operation: `activate`, `focus`, `fill`, `set_checked`, `set_active`, `set_value`, `set_expanded`, `select`, `select_page`, `popup`, `popdown`, `scroll_to`, `scroll_by`, `close`, `play`, `pause`, `seek`. The operations a widget supports are listed in its `capabilities` |
| `inspect_text` | R | Text buffer ranges, cursor, selection, tag spans |
| `edit_text` | M | Insert, delete or replace ranges; set the selection |
| `wait_for` | R | Wait, with a time limit, for a widget to exist, a match count, a property, text, mapped, sensitive, or `GApplication:is-busy`. Sends progress updates and can be cancelled. On timeout it returns `satisfied:false` |
| `batch` | M* | Run steps in order. All steps are checked before the first one runs. At most 100 steps, no nested batches, no lifecycle steps. One main-loop pass between steps. `$N.ref` means step N's target. Stops at the first error by default, and nothing is rolled back. *A batch counts as read-only when all its steps are read-only |
| `inspect_model` | R | Items of list and tree models, page by page, with selection and sort/filter chains. Row widgets that exist are listed separately. Rows are matched to items only through app adapters |
| `capture` | R | Render a widget, window or popover to PNG through GTK. Settles by default: clean, unchanged natives need no new frame; after temporary CSS changes, the first settled capture of each native requests a fresh frame and waits for theme transitions to become quiet. If the clock stalls or keeps animating past the limit, returns an earlier capture with `stale:true` and `stale_reason:"no_frame"` or `"animating"`, or fails if none is available. Window captures leave out popovers. Refuses secret fields that are showing their text. Returns `capture_id` |
| `diff_captures` | R | Pixel diff of two kept captures: diff image, changed regions, ratio. A size mismatch is reported as such |

## actions

| Tool | | Purpose |
|---|---|---|
| `list_actions` | R | Actions for app, window or widget scope, with their source and `complete` (false when an inserted group was not registered) |
| `inspect_action` | R | Parameter type, state type and value, enabled (`null` for widget-class actions), state hint |
| `activate_action` | M | Activate with a typed parameter |
| `change_action_state` | M | Ask for a state change and return the state read back |
| `inspect_action_binding` | R | Action name and target of a widget that triggers an action |
| `inspect_menu` | R | Menu model sections, submenus, actions, targets |
| `inspect_shortcuts` | R | Triggers and actions from shortcut controllers, including managed and global scopes, plus application accelerators when `target` is omitted. `max_items` bounds the combined entries (default 200); returns `total` and `truncated` |
| `activate_shortcut` | M | Match a trigger such as `<Control>s` against shortcut controllers, from the focused widget upward plus the managed and global ones, then run its `GtkShortcutAction`. This compares keyval and modifiers only; it is not a key press |
| `inspect_controllers` | R | Event controllers and gestures: phase and configuration |

`inspect_shortcuts` counts each controller in `controllers` and each action/accelerators entry in `app_accels` as one item. Controllers use the budget first; application accelerators use the remainder. `total` counts both kinds before limiting, and `truncated: true` means some entries were omitted. `max_items: 0` returns no entries but still reports the total. A `target` limits the widget subtree and leaves out `app_accels`.

## layout-style

| Tool | | Purpose |
|---|---|---|
| `inspect_layout` | R | Measured size (optionally `for_size`), allocation, baseline, margins, alignment, expand flags, parent and sibling context |
| `audit_layout` | R | Finds ellipsized `GtkLabel`s, children outside their parent, visible zero-size widgets, overlapping siblings, and icon names that don't resolve. `GtkInscription` ellipsis is reported as unsupported |
| `inspect_style` | R | CSS name, classes, state flags, public style information |
| `apply_css` | M | Add a temporary CSS provider. Returns parse errors with their location, and a handle. Removed by `remove_css` or when the client disconnects |
| `remove_css` | M | Remove one temporary provider |
| `highlight` | M | Draw an outline around a widget for a person to see, using a temporary provider and the class `gtk-mcp-highlight`. The agent hides this class from snapshots, diffs and selectors |
| `inspect_render` | R | GSK render-node tree, size-limited. Text inside secret subtrees is replaced |
| `inspect_settings` | R | GTK settings and the libadwaita style manager |
| `override_setting` | M | Typed override for this session: color scheme, animations, text scale, text direction. libadwaita high contrast and accent can only be read |

Details:

- `inspect_layout`: with `for_size` it measures twice. `as_width` is the height needed at a width of `for_size` (vertical measure). `as_height` is the horizontal measure. Detached targets fail with `detached`.
- `audit_layout`: checks mapped widgets only. `checks` picks the rules and `limit` caps the findings (`total`, `truncated`). The geometry rules skip GTK/libadwaita gizmos, scrolling containers, windows and collapsed revealers. `GtkInscription` ellipsis goes into `unsupported`, not `findings`.
- `apply_css`: the provider is added at `GTK_STYLE_PROVIDER_PRIORITY_USER`. Lines and columns start at 1. Invalid CSS returns `installed:false, handle:null` with the errors and installs nothing. Warnings don't block it. `apply_css`, `remove_css` and `highlight` request layout and drawing on every mapped native (windows and popovers), then wait with a limit for each clock's frame counter to advance past the change and remain quiet for at least 40 ms. This includes theme transitions that start when a provider is removed. `settled:true` means all mapped natives reached that quiet state, or nothing is mapped. Otherwise `settled:false` has `settled_reason:"no_frame_clock"`, `"no_frame"`, `"animating"` or `"cancelled"`; `"animating"` means a frame arrived but the clock did not become quiet within the budget. A missing clock on a mapped native never counts as settled. `settled_reason` is null on success. The first settled `capture` of each native after a provider change also requests a fresh frame and waits for the same quiet period rather than trusting a cached render node. A capture whose CSS transitions outlast its budget returns an earlier frame with `stale:true, stale_reason:"animating"`, or fails `capture_unavailable` if no safe earlier frame exists. Later unchanged captures take the fast path. Handles belong to the connection that made them. Disconnecting or closing the app removes all of them.
- `inspect_render`: text nodes carry the glyph count, font and colour. The GTK bindings don't expose the characters. Text nodes that overlap a secret widget become `{"type":"text","redacted":true}`. Every other node inside a secret widget keeps only its type (`redacted: true`), because the caret, selection and scroll positions would give away the length. The call is refused while a secret is showing its text.
- `override_setting`: `setting` is `color_scheme`, `animations`, `text_scale` (a factor of `gtk-xft-dpi` before the override) or `text_direction`. `reset:true` restores one setting. Overrides are undone when the last client holding them disconnects.

## a11y

AT-SPI is the Linux accessibility bus that screen readers use. The broker reads it when started with `--allow-atspi` and links what it finds to widget refs. Without that flag, AT-SPI fields are reported as unavailable.

| Tool | | Purpose |
|---|---|---|
| `inspect_accessible` | R | From GTK: role, accessible parent/children, bounds, platform state. From AT-SPI: name, description, states, relations, actions, text/value. Each field says where it came from |
| `accessible_tree` | R | The accessible tree, linked to widget refs. AT-SPI nodes that can't be linked have `ref: null` |
| `audit_accessibility` | R | Rule findings with ref and evidence. Rules that need AT-SPI are reported as skipped when it's off |
| `trace_focus_order` | M | Walk the Tab order with `gtk_widget_child_focus` and report the order, loops and widgets that can't be reached. Puts focus back afterwards. Moving focus can have side effects: scrolling to the focused widget, popovers closing, text getting selected on focus |

GTK values come from the public `GtkAccessible` API (role, parent/children, bounds, platform state). AT-SPI values (name, description, states, relations, actions, text, value, attributes) come from the accessibility bus. In `accessible_tree`, a node is `source: "gtk+atspi"` when both sides were linked. It is `"gtk"` when GTK doesn't export the node (role `none`/`presentation`, list-view internals). It is `"atspi"` with `ref: null` when the node exists only on AT-SPI. Without `--allow-atspi`, or when the app is not on the accessibility bus, AT-SPI fields read `{"available": false, "reason"}` and AT-SPI audit rules report `status: "skipped"`. Secret widgets never show their name, text or value.

Nodes also carry the widget facts the audit rules need (label, tooltip, icon name, sensitivity, mapping), plus `clickable: true` when the application added press-based gesture controllers (`GtkGestureClick` and other `GtkGestureSingle` subclasses) beyond the widget type's GTK defaults. The agent caches controller counts by GType using a throwaway instance; GTK's own selection/link gestures on labels are excluded too. Native/root widgets and types that cannot be instantiated are not classified as app-clickable. Private GTK list rows and unnamed implementation children of spin buttons, dropdowns and expanders are excluded because their owning composite installs gestures after initialization; explicitly identified app children and an expander's public content/label remain eligible. This is a controller-baseline heuristic, not gesture provenance tracking. `audit_accessibility` treats app-clickable widgets as interactive even when their accessible role is not (tray icons): they get the keyboard-reach (`interactive_not_focusable`) and target-size (`small_target`) checks, and `clickable_without_role` flags them with the advice to give the widget role `button` and a label.

## diagnostics

| Tool | | Purpose |
|---|---|---|
| `watch` | R | Watch property changes, model and selection changes, supported signals, and AT-SPI events. Secret objects report names only |
| `read_events` | R | Read the event stream by cursor (including `late_result` records). Reports dropped events |
| `unwatch` | R | Remove a subscription |
| `read_logs` | R | Captured stderr of apps the broker launched, plus records from apps that call `forward_log`. Otherwise `unavailable` |
| `measure_frames` | R | Frame-clock timings over a limited time window |
| `object_stats` | R | Instance counts per type. `unavailable` unless the app started with `GOBJECT_DEBUG=instance-count` |
| `record` | R | Start recording interactions (input-attributed emission hooks, `plan.md` 5.6) |
| `stop_recording` | R | Return a `batch` script you can replay, with selectors and placeholders for secrets. Emissions the app made by itself are listed separately |

Details:

- `watch` takes a `target` and a `kind`:
  - `notify` watches `properties` (every property by default). `GObject::notify` can't be hooked, so this uses `connect_notify_local`.
  - `signal` watches one `signal` through a shared emission hook filtered to the target. `notify` and `G_SIGNAL_NO_HOOKS` signals are refused.
  - `model` watches `items-changed`, plus `selection-changed` for selection models. The target is a model or a widget with a `model` property.

  It returns a `subscription` id. Subscriptions belong to the connection and go away when it disconnects. There can be at most 256.
- `watch {kind: "atspi", events?: ["object:state-changed:focused"]}` has no `target`. It watches the whole app through the broker's AT-SPI source and needs `--allow-atspi`. `events` filters by the start of the event name. If it is left out or empty, you get focus, state, accessible-name and window events. Each session has one AT-SPI subscription, called `"atspi"`; a new AT-SPI watch replaces it. `unwatch {subscription: "atspi"}` stops it, even after the app has exited. Records in `read_events` look like `{kind: "atspi", source: "atspi", subscription: "atspi", event, detail1, detail2, path, ref, role, name}`. `ref` is the linked widget ref, or null. Secret widgets, and every AT-SPI node below them, leave out `name` and carry `secret: true`. They keep `detail1`/`detail2` only for state, focus and window events, because text events carry offsets and lengths. A node that hasn't been linked (yet) might be secret, so it carries `unresolved: true` and a null `ref`/`role`, and is held back the same way. The subscription and kind filters work on these records just as they do on agent events.
- Events arrive as `$/event` notifications and are kept in a fixed-size buffer per session: `{kind: notify|signal|items_changed|selection_changed, subscription, target, type, property|signal|position…, value|args, time_ms, by_agent}`. Events of secret objects have no `value`/`args`, only `secret: true` and the name. `by_agent` is true when the agent's own request caused the change. `read_events {since, limit, subscription, kind}` returns `{entries[{cursor,…}], next_cursor, more, dropped, dropped_total}`. `late_result` records show up as `kind: late_result`. This also works for sessions whose app has exited.
- `read_logs {since, limit, min_level, contains, source}` reads the session's log buffer. It holds stdout/stderr lines of apps the broker launched; lines from GLib's default writer are split into `level`, `domain` and `message`. It also holds `source: forwarded` records from `gtk_mcp_agent::forward_log(level, fields)` (`level`, `domain`, `message`, `fields`). An app the broker didn't launch gets `unavailable` if it never forwarded anything and doesn't advertise `LogForwarding`. It counts as forwarding once it has advertised that capability (by calling `forward_log` before the handshake) or sent a `$/log` record. Records forwarded while no broker is connected are kept (up to 512) and delivered at the handshake. `min_level` drops lines that have no severity.
- `measure_frames {target, duration_ms}` (default 1000, max 10000) listens to the target's frame clock (by default the active window). `frames` counts paint callbacks, `paint_fps` measures their rate between the first and last frame times, and `average_paint_fps_over_window` divides their count by the measurement duration. Shared GDK timings are retained for every paint, rather than looked up after older entries have left GDK's bounded history. After the paint window closes, collection allows up to 250 ms for incomplete compositor feedback; this does not extend `window_ms` or change paint-rate accounting. `presented_fps` measures the rate between distinct positive presentation timestamps from complete compositor timings, deduplicating repeated timestamps; it is null without at least two distinct presentations. `refresh_hz` comes only from the timings' positive refresh intervals and is null otherwise, with `refresh_unavailable_reason` explaining why. Positive presentation timestamps do not imply a known refresh interval: nested compositors can present at the host's cadence while reporting zero refresh intervals, and their advertised mode is not a bound on presentation rate. Paint rate can exceed presentation rate. The result also includes paint interval statistics (min/avg/p95/p99/max), `missed_frames` (paint intervals longer than 1.5× the refresh interval), long frames, and timings for the first 120 paints. Each listed timing includes `complete`, `raw_presentation_time_us`, and `refresh_interval_us` so missing, incomplete, and complete-but-zero samples can be distinguished; `presentation_time_us` includes only positive timestamps from complete samples. If the compositor does not report a refresh interval, missed-frame classification and `refresh_interval_ms` use 16.667 ms, marked by `refresh_interval_reported: false`; this fallback does not supply `refresh_hz`. `presentation.available`, `frames_with_timing`, and `distinct_presentations` describe the available presentation samples; `presentation.complete_frames` counts complete timings, including those without a presentation timestamp. With no positive complete presentation timestamps, `presentation.status` is `"unavailable"` and `presentation.reason` distinguishes absent timestamps from timings that did not complete before the deadline. `presentation.frame_to_presentation` contains nonnegative latency statistics or null when there are no valid samples. GDK defines frame and presentation times in the same monotonic clock domain; samples before their frame time or outside the monotonic observation window are excluded and counted in `presentation.latency_samples_discarded`, with an explanation in `presentation.latency_note`. Missing presentation timestamps do not count as discarded latency samples. A surface that is not redrawing reports `frames: 0, idle: true`.
- `record {}` / `stop_recording {}`: one recording per app, owned by the connection that started it. `record` initializes signal classes even if the app has never created those widgets and returns `{recording, hooks, skipped_hooks, controllers, human_window_ms}`. `hooks` lists installed interaction signal names; `skipped_hooks` lists signals that remain unavailable or cannot be hooked, including `GtkWidget::map` if that hook fails. An unavailable hook does not abort the recording. Input listeners go on every toplevel and every popover, because popovers are separate surfaces, and on windows and popovers that appear later (through a hook on `GtkWidget::map`). Opening and closing a menu button's popover is recorded as `popup`/`popdown` on the menu button. `stop_recording` returns `{batch: {steps, stop_on_error}, steps, app_emissions, secrets, human_emissions, ignored_agent_emissions, dropped_events, duration_ms}`. Back-to-back `fill`/`set_value`/`set_*` steps on the same widget are merged into one. `GSimpleAction::activate`/`change-state` become `activate_action` / `change_action_state` with `win.`/`app.` names. An action in an unregistered group is skipped. A `change-state` right after an `activate` of the same action is treated as its result and dropped. A signal is counted as human when it comes within 300 ms of real input. That is a rule of thumb: an app signal that happens to fire right after real input is reported as `human`.

## app

| Tool | | Purpose |
|---|---|---|
| `app_open` | M | `g_application_open` with files, or `activate` |
| `inspect_gsettings` | R | The app's GSettings schemas, keys and values |
| `set_gsetting` | M | Write a key in the app's own schemas (those registered with `register_settings` or under its application id). Other schemas are read-only. A relocatable schema is written only at the path of a registered settings object, or under the app's path (`/` + app id with `.` as `/`, any case) |
| `list_resources` | R | Registered GResource paths |
| `read_resource` | R | Read one resource, size-limited |

Besides tools, the app can share debug state with `register_state` (`embedding.md`), for example `app-state/panels`. Read it as the MCP resource `gtk-mcp://<session>/app-state/panels` or with `gtk-mcp state panels`; `gtk-mcp state` lists the available names and apps.

Totals: 56 tools, 17 of them mutating (`batch` counts as mutating).

## Excluded

See `plan.md` section 11.
