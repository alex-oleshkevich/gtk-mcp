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
  `{"type":"text","redacted":true}`; peeking secrets refuse the call.
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

## app

| Tool | | Purpose |
|---|---|---|
| `app_open` | M | `g_application_open` with files, or `activate` |
| `inspect_gsettings` | R | The app's GSettings schemas, keys, values |
| `set_gsetting` | M | Write a key |
| `list_resources` | R | Registered GResource paths |
| `read_resource` | R | Read one resource, size-limited |

MCP resources (not tools): debug state registered by the app through
`register_state` (`embedding.md`), e.g. `app-state/panels`.

Totals: 56 tools, 17 mutating (`batch` counted as mutating).

## Excluded

See `plan.md` section 11.
