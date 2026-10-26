# Tools

Status: final, aligned with `plan.md` (2026-10-26).

Scope: GTK, GDK, GSK, GIO application state, and libadwaita, inspected and
exercised from inside the application process. Real input, desktop capture,
clipboard, window placement, and compositor state belong to `../wayland-mcp`.

`R` = read-only. `M` = mutating: needs broker `--allow-mutations` **and**
`GTK_MCP_MUTATIONS=1` in the app; otherwise `mutations_disabled`. Lifecycle
tools (`launch_app`, `restart_app`, `stop_app`) need broker `--allow-launch`
instead. Tool annotations follow the same split.

Exposure: broker `--toolsets` (default `core`), refined by `--tools` /
`--exclude-tools`; unknown names fail startup.

Targets are a ref (`w12`, `o4`) or a selector (`plan.md` section 7). Actions
are addressed as `{scope, name}`. Mutating tools accept `observe` and return
the post-action state; when dispatch succeeds but observation fails, both are
reported and nothing is retried.

## core

| Tool | | Purpose |
|---|---|---|
| `list_apps` | R | Sessions: PID, app ID, GTK/libadwaita versions, capabilities, `mutations_enabled`, state (`running`, `incompatible`, `exited` with code/signal, in-flight request, retained logs/events), `launched_by_broker` |
| `list_windows` | R | Toplevels and mapped native surfaces (popovers): title, visibility, size, refs, renderer, surface scale, monitor connector, modal / transient-for / default widget, layer-shell configuration when gtk4-layer-shell is loaded |
| `launch_app` | M | Broker-launched argv (no shell) with GTK presets (`GDK_DEBUG=no-portals`, `GOBJECT_DEBUG=instance-count`, `GTK_A11Y`, `LANGUAGE`, `GSK_RENDERER`, `GTK_DEBUG`), optional `WAYLAND_DISPLAY`; captures stdout/stderr. Caller-supplied `GTK_MCP*` rejected. Presets are inherited by the app's own children; `no-portals` also disables portal OpenURI |
| `restart_app` | M | Broker-launched sessions only; new session, old refs invalid |
| `stop_app` | M | Broker-launched sessions only; SIGTERM, then SIGKILL after timeout |
| `snapshot` | R | Widget tree, `full` or `semantic`, `text` or `json` (`widget-tree.md`); returns `snapshot_id` |
| `diff_snapshots` | R | Compare two retained snapshots by `snapshot_id`; changes reported per widget ref |
| `query` | R | Resolve a selector to all matching refs |
| `inspect` | R | Type, ancestry, identities, state (`live` / `detached`), capabilities, static tooltip, related objects as refs |
| `list_properties` | R | Property schemas, filterable by owner class or name |
| `get_property` | R | Read values; object values as refs; secret objects redacted |
| `set_property` | M | Write a property; returns the observed value |
| `list_signals` | R | Signal schemas and watch eligibility |
| `interact` | M | Typed semantic operation: `activate`, `focus`, `fill`, `set_checked`, `set_active`, `set_value`, `set_expanded`, `select`, `select_page`, `popup`, `popdown`, `scroll_to`, `scroll_by`, `close`, `play`, `pause`, `seek`. Supported operations per widget are listed in `capabilities` |
| `inspect_text` | R | Text buffer ranges, cursor, selection, tag spans |
| `edit_text` | M | Insert, delete, replace ranges; set selection |
| `wait_for` | R | Bounded wait for existence, match count, property, text, mapped, sensitive, or `GApplication:is-busy`; progress notifications; cancellable; timeout returns `satisfied:false` |
| `batch` | M* | Ordered steps, all validated before the first dispatch; ≤100 steps; no nesting or lifecycle steps; one main-loop iteration between steps; `$N.ref` refers to step N's target; stop at first error by default; no rollback. *Classified from its steps: read-only batches run without mutations |
| `inspect_model` | R | Paginated items of list/tree models, selection, sort/filter chains; realized row widgets listed separately; row↔item mapping only through app adapters |
| `capture` | R | Render a widget, window, or popover to PNG via GTK. Settles by default (one `after-paint` after the request, bounded); otherwise `stale: true` with reason. Window captures exclude popovers. Refuses peeking secret fields. Returns `capture_id` |
| `diff_captures` | R | Pixel diff of two retained captures: diff image, changed regions, ratio; size mismatch explicit |

## actions

| Tool | | Purpose |
|---|---|---|
| `list_actions` | R | Actions for app, window, or widget scope, with source and `complete` (false for unregistered inserted groups) |
| `inspect_action` | R | Parameter type, state type/value, enabled (`null` for widget-class actions), state hint |
| `activate_action` | M | Activate with a typed parameter |
| `change_action_state` | M | Request a state change; returns observed state |
| `inspect_action_binding` | R | Action name and target of an actionable widget |
| `inspect_menu` | R | Menu model sections, submenus, actions, targets |
| `inspect_shortcuts` | R | Triggers and actions from shortcut controllers, including managed and global scopes |
| `activate_shortcut` | M | Match a keyval trigger (e.g. `<Control>s`) against shortcut controllers from the focus widget upward plus managed/global ones, then run its `GtkShortcutAction`. Keyval/modifier comparison only; not a key press |
| `inspect_controllers` | R | Event controllers and gestures: phase, configuration |

## layout-style

| Tool | | Purpose |
|---|---|---|
| `inspect_layout` | R | Measurement (optionally `for_size`), allocation, baseline, margins, alignment, expand flags, parent/sibling context |
| `audit_layout` | R | Ellipsized `GtkLabel`s, children outside parent bounds, visible zero-size widgets, overlapping siblings, unresolvable icon names. `GtkInscription` ellipsis reported unsupported |
| `inspect_style` | R | CSS name, classes, state flags, public style information |
| `apply_css` | M | Temporary CSS provider; parse diagnostics with location; handle; reverted on `remove_css` or client disconnect |
| `remove_css` | M | Remove one temporary provider |
| `highlight` | M | Outline a widget for a human through a temporary provider and the class `gtk-mcp-highlight`; the agent hides its own class from snapshots, diffs, and selectors |
| `inspect_render` | R | GSK render-node tree, size-limited; text inside secret subtrees replaced |
| `inspect_settings` | R | GTK settings, libadwaita style manager |
| `override_setting` | M | Session-scoped typed override: color scheme, animations, text scale, text direction. libadwaita high contrast and accent are read-only |

As built (M3):

- `inspect_layout`: `for_size` measures twice: `as_width` (height requested for
  a width of `for_size`, vertical measure) and `as_height` (horizontal measure).
  Detached targets fail `detached`.
- `audit_layout`: judges mapped widgets only; `checks` selects rules, `limit`
  caps findings (`total`, `truncated`). GTK/libadwaita gizmos, scrolling
  containers, windows and collapsed revealers are skipped by the geometry
  rules. `GtkInscription` ellipsis lands in `unsupported`, not `findings`.
- `apply_css`: provider at `GTK_STYLE_PROVIDER_PRIORITY_USER`, lines/columns
  1-based. Invalid CSS returns `installed:false, handle:null` with diagnostics
  and installs nothing; warnings do not block. A restyle reaches the next
  frame, so `apply_css`, `remove_css` and `highlight` wait (bounded, `settled`)
  for a frame on every mapped window; a later `capture` then shows the result.
  Handles belong to the connection that made them; disconnect and app shutdown
  remove all of them.
- `inspect_render`: text nodes carry glyph count, font and colour (the bindings
  expose no characters). Text nodes overlapping a secret widget are replaced by
  `{"type":"text","redacted":true}`; every other node inside a secret widget
  keeps only its type (`redacted: true`), since caret, selection and scroll
  geometry encode the length. Peeking secrets refuse the call.
- `override_setting`: `setting` is `color_scheme`, `animations`, `text_scale`
  (factor of the pre-override `gtk-xft-dpi`) or `text_direction`; `reset:true`
  restores one setting. Overrides are reverted when the last client holding
  them disconnects.

## a11y

AT-SPI data is fetched by the broker (`--allow-atspi`) and joined to widget
refs; without it, AT-SPI fields are reported unavailable.

| Tool | | Purpose |
|---|---|---|
| `inspect_accessible` | R | GTK: role, accessible parent/children, bounds, platform state. AT-SPI: name, description, states, relations, actions, text/value. Each field tagged with its source |
| `accessible_tree` | R | Accessible hierarchy linked to widget refs; unjoined AT-SPI nodes have `ref: null` |
| `audit_accessibility` | R | Rule findings with ref and evidence; AT-SPI-dependent rules reported as skipped when the source is off |
| `trace_focus_order` | M | Walk the Tab chain with `gtk_widget_child_focus`, report order, loops, unreachable widgets; restores focus. Side effects possible: scroll-to-focus, popover autohide, select-on-focus |

Fields carry their source: GTK-sourced values come from the public `GtkAccessible`
API (role, parent/children, bounds, platform state); AT-SPI-sourced values
(name, description, states, relations, actions, text, value, attributes) come
from the a11y bus. `accessible_tree` nodes are `source: "gtk+atspi"` when
joined, `"gtk"` when GTK does not export them (role `none`/`presentation`,
list-view internals) and `"atspi"` with `ref: null` for AT-SPI-only nodes.
Without `--allow-atspi` (or when the app is not on the a11y bus) AT-SPI fields
read `{"available": false, "reason"}` and AT-SPI audit rules `status: "skipped"`.
Secret widgets never expose name, text or value.

## diagnostics

| Tool | | Purpose |
|---|---|---|
| `watch` | R | Property notifications, model/selection changes, supported signals, AT-SPI events; secret objects report names only |
| `read_events` | R | Event stream by cursor (includes `late_result` records), reports drops |
| `unwatch` | R | Remove a subscription |
| `read_logs` | R | Captured stderr of broker-launched apps, and records forwarded by apps that call `forward_log`; otherwise `unavailable` |
| `measure_frames` | R | Frame-clock timings over a bounded window |
| `object_stats` | R | Per-type instance counts; `unavailable` unless the app started with `GOBJECT_DEBUG=instance-count` |
| `record` | R | Start recording interactions (input-attributed emission hooks, `plan.md` 5.6) |
| `stop_recording` | R | Return a replayable `batch` script with selectors and secret placeholders; app-originated emissions listed separately |

As built (M5/M6):

- `watch` takes `target` plus `kind`: `notify` (`properties`, default every
  property; `GObject::notify` is not hookable, so this uses
  `connect_notify_local`), `signal` (`signal`; a shared emission hook filtered
  to the target; `notify` and `G_SIGNAL_NO_HOOKS` signals are refused) or
  `model` (`items-changed`, plus `selection-changed` for selection models; the
  target is a model or a widget with a `model` property). It returns a
  `subscription` id. Subscriptions belong to the connection and disappear on
  disconnect; at most 256.
- `watch {kind: "atspi", events?: ["object:state-changed:focused"]}` omits
  `target` and watches the whole app through the broker's AT-SPI source
  (`--allow-atspi` required). `events` filters by event-name prefix; omitted
  or empty selects focus, state, accessible-name and window events. One
  subscription (`"atspi"`) exists per session; another AT-SPI watch replaces
  it. `unwatch {subscription: "atspi"}` stops it, including after app exit.
  Records in `read_events` have `{kind: "atspi", source: "atspi",
  subscription: "atspi", event, detail1, detail2, path, ref, role, name}`;
  `ref` is the joined widget ref or null. Secret widgets (and every AT-SPI node
  below them) omit `name` and carry `secret: true`; their `detail1`/`detail2`
  are kept only for state, focus and window events, since text events carry
  offsets and lengths. Nodes the join does not know (yet) may be secret: they
  carry `unresolved: true`, null `ref`/`role`, and are withheld the same way.
  The subscription and kind filters work for these broker-sourced records as
  for agent events.
- Events are `$/event` notifications kept in the session's bounded ring:
  `{kind: notify|signal|items_changed|selection_changed, subscription, target,
  type, property|signal|position…, value|args, time_ms, by_agent}`. Events of
  secret objects have no `value`/`args`, only `secret: true` and the name.
  `by_agent` is true when the agent's own request caused the change.
  `read_events {since, limit, subscription, kind}` returns
  `{entries[{cursor,…}], next_cursor, more, dropped, dropped_total}`;
  `late_result` records appear as `kind: late_result`. It works on exited
  sessions.
- `read_logs {since, limit, min_level, contains, source}` reads the session's
  log ring: stdout/stderr lines of broker-launched apps (GLib default-writer
  lines are parsed into `level`, `domain`, `message`) and `source: forwarded`
  records from `gtk_mcp_agent::forward_log(level, fields)` (`level`, `domain`,
  `message`, `fields`). `unavailable` for an external app that never forwarded
  and does not advertise `LogForwarding`; the session counts as forwarding once
  it advertised the capability (a `forward_log` call before the handshake) or
  sent a `$/log` record. Records forwarded while no broker is connected are
  kept (512) and delivered at the handshake. `min_level` drops lines without a
  severity.
- `measure_frames {target, duration_ms}` (default 1000, max 10000) connects to
  the target's frame clock (default: active window) for the window and reports
  frames, fps, interval statistics (min/avg/p95/p99/max), `missed_frames`
  (interval above 1.5× refresh), long frames and per-frame timings (first
  120). Presentation times come from the clock history when the compositor
  reported them (`presentation.available`). An idle surface reports
  `frames: 0, idle: true`.
- `record` / `stop_recording`: one recording per app, owned by the connection
  that started it. Controllers go on every toplevel and popover (spike S4:
  popovers are separate natives) and on windows/popovers mapped later (a hook on
  `GtkWidget::map`). Menu-button popover toggles are recorded as
  `popup`/`popdown` on the menu button. `stop_recording` returns
  `{batch: {steps, stop_on_error}, steps, app_emissions, secrets,
  human_emissions, ignored_agent_emissions, dropped_events, duration_ms}`.
  Consecutive `fill`/`set_value`/`set_*` steps on the same widget are merged;
  `GSimpleAction::activate`/`change-state` become `activate_action` /
  `change_action_state` with `win.`/`app.` names (an action in an unregistered
  group is skipped; a `change-state` right after the `activate` of the same
  action is its consequence and dropped). The 300 ms human window is a
  heuristic: an app emission right after real input is reported as `human`.

## app

| Tool | | Purpose |
|---|---|---|
| `app_open` | M | `g_application_open` with files, or `activate` |
| `inspect_gsettings` | R | The app's GSettings schemas, keys, values |
| `set_gsetting` | M | Write a key of the app's own schemas (registered with `register_settings` or under its application id); other schemas are read-only. A relocatable schema is written only at a registered object's path or under the app's path (`/` + app id with `.` as `/`, any case) |
| `list_resources` | R | Registered GResource paths |
| `read_resource` | R | Read one resource, size-limited |

MCP resources (not tools): debug state registered by the app through
`register_state` (`embedding.md`), e.g. `app-state/panels`.

Totals: 56 tools, 17 mutating (`batch` counted as mutating).

## Excluded

See `plan.md` section 11.
