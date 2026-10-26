# Widget Tree Snapshot

Status: final, aligned with `plan.md` (2026-10-26).

## Purpose

The widget tree is the primary observation surface of `gtk-mcp`. It lets an
agent discover what a GTK application currently contains, choose a target,
and then call `inspect`, `interact`, `inspect_layout`, or `capture` with an
unambiguous reference.

It is a GTK widget tree, not an accessibility tree and not a Wayland surface
tree:

- Internal GTK containers and non-actionable widgets are present in the full
  view.
- Event controllers, actions, models, and other non-widget objects are not
  tree children. They are reached through `inspect` from the widget that owns
  or exposes them.
- Popovers are parented children of their owner widget in GTK 4, so they
  appear in place, marked `native: true`. They are not repeated as separate
  roots; `list_windows` lists mapped native surfaces separately.
- Content a widget holds but has unparented (a collapsed `GtkExpander`'s
  child) appears through detached-content adapters, marked
  `attached: false` (see "Detached content").

## Snapshot tool

```json
{
  "tool": "snapshot",
  "session": "s1",
  "root": {"window": {"monitor": "DP-1"}},
  "view": "full",
  "format": "text",
  "max_depth": 12,
  "max_nodes": 500,
  "include_unmapped": true,
  "include_closed_popovers": false,
  "text_limit": 128
}
```

| Field | Meaning |
|---|---|
| `session` | Instrumented application session. Required. |
| `root` | A target (`plan.md` section 7): ref, or selector whose `window` key uses `WindowSel`. Omitted: all toplevels in the session. |
| `view` | `full` or `semantic`. Default: `full`. |
| `format` | `text` or `json`. Default: `text`. JSON is canonical; text is derived from the same data. |
| `max_depth` | Maximum descendant depth below the requested root. |
| `max_nodes` | Hard node budget. Truncation is reported, never silent. |
| `include_unmapped` | Include widgets that exist in the hierarchy but are not mapped. Default: `true`. |
| `include_closed_popovers` | Expand subtrees of unmapped popovers. Default: `false`: a closed popover appears as one node with `children_collapsed: true` and its child count. |
| `text_limit` | Maximum characters of label/text content per node. Truncation is reported. |

### Views

**`full`** — every widget in the hierarchy reachable through
`gtk_widget_get_first_child` / `gtk_widget_get_next_sibling`, in child order,
plus detached content from adapters, up to the requested limits.

**`semantic`** — a compressed view for task-oriented work. It keeps
actionable, text-bearing, selectable, and state-bearing widgets, plus the
ancestors needed to preserve hierarchy. Purely structural or decorative
widgets may be elided. Elision is marked in JSON (`elided_children`) and in
text (`… 3 structural widgets`). Semantic view never invents nodes and never
changes parentage of the nodes it keeps: an elided node's kept descendants
stay attached to their nearest kept ancestor, and the elision marker records
that the path is compressed. A node cut by `max_depth` or `max_nodes` is
always kept, so its `children_truncated` marker survives.

The default remains `full`: this is a GTK development tool, and layout bugs
usually live in the structural widgets a semantic view would hide.

### Detached content

Some widgets unparent content while hidden and keep a reference to it. The
agent lists such content under its holder through per-type adapters, read via
public properties:

| Holder | Source | Note |
|---|---|---|
| `GtkExpander` | `child` property | Unparented while collapsed |

Adapter nodes carry `attached: false` and `via: "GtkExpander:child"`. Their
descendants are walked normally. New adapters are added only for verified
GTK behavior.

## Canonical JSON form

```json
{
  "snapshot_id": "snap-1042",
  "session": "s1",
  "sequence": 1042,
  "roots": [
    {
      "ref": "w1",
      "type": "GtkWindow",
      "root_kind": "window",
      "title": "Preferences",
      "state": {"visible": true, "mapped": true, "sensitive": true},
      "bounds_in_root": {"x": 0, "y": 0, "width": 480, "height": 640},
      "children": []
    }
  ],
  "node_count": 1,
  "complete": true,
  "truncated": false
}
```

### Node fields

Required for every node:

| Field | Meaning |
|---|---|
| `ref` | Session-scoped opaque reference, e.g. `w42`. See "References". |
| `type` | GType name, e.g. `GtkExpander`, `AdwExpanderRow`, `GtkListView`. |
| `children` | Child nodes in GTK child order, then detached content. Empty array for leaves. |

Present when GTK exposes the information:

| Field | Meaning |
|---|---|
| `identity.buildable_id` | `GtkBuildable` ID, when set. On GTK ≥ 4.22 this is also the widget's AT-SPI `AccessibleId`. |
| `identity.automation_id` | The widget name, when the application set one explicitly (`gtk_widget_set_name`; GTK returns the type name when none is set, and that case is omitted). Needs no agent dependency in app code. See `embedding.md`. |
| `identity.css_name` | Result of `gtk_widget_get_css_name`. |
| `identity.css_classes` | Result of `gtk_widget_get_css_classes`, minus agent-owned classes (`gtk-mcp-highlight`). |
| `role` | Accessible role from `gtk_accessible_get_accessible_role`. This annotates the node; it does not turn the tree into an accessibility tree. |
| `a11y.name` | Accessible name. GTK has no public in-process getter for accessible properties, so this is present only when the broker's AT-SPI source is enabled and the node is joined, marked `"source": "atspi"`. Kept distinct from widget text. |
| `label` | Human-readable label (button label, expander label, row title) when an adapter can read it. |
| `text` | Text content for textual widgets. See "Text and redaction". |
| `value` | For range-like widgets: `{"current": …, "min": …, "max": …}`. |
| `state` | Booleans that apply: `visible`, `mapped`, `sensitive`, `focusable`, `has_focus`, `expanded`, `active`, `checked`, `selected`. Inapplicable states are omitted, not reported as `false`. |
| `native` | `true` for popovers and other native surfaces inside the tree. |
| `attached` | `false` for detached content; omitted otherwise. `via` names the adapter. |
| `children_collapsed` | Closed popover whose subtree was not expanded; includes `child_count`. |
| `children_source` | `"realized"` on list views: children are the row widgets that exist, not the model. |
| `allocation` | `{"width": …, "height": …}` from the current allocation. |
| `bounds_in_root` | Rectangle relative to the snapshot root via `gtk_widget_compute_bounds`, valid across popover boundaries. Omitted with a node-level warning when it fails. |
| `capabilities` | Semantic operations this node supports, e.g. `["set_expanded", "activate", "focus"]`. Consumed by `interact`. |
| `elided_children` | Semantic view only: number of nodes compressed away below this node. |
| `secret` | `true` when the node is in the secret set (`plan.md` 4.4). |
| `warnings` | Node-level problems, e.g. bounds unavailable, text unreadable. |

Unknown or version-specific data is not guessed. Additional data goes under
a namespaced key (`adw`, `a11y`) announced by handshake capabilities, not by
changing core fields.

### Text format

Text is a lossless-in-structure, lossy-in-content rendering of the JSON:
hierarchy, order, type, reference, key state, and truncated text survive;
long strings and class lists may be shortened.

```text
GtkWindow "Preferences" [w1] 480x640 mapped
└── AdwNavigationView [w2]
    └── GtkBox [w3]
        ├── GtkExpander "Advanced options" [w8] expanded=false
        │   └── GtkEntry [w9] detached via=GtkExpander:child mapped=false text="localhost"
        ├── GtkPasswordEntry [w11] secret text=<redacted>
        ├── GtkMenuButton [w13]
        │   └── GtkPopoverMenu [w14] native closed (3 children)
        ├── GtkSwitch [w10] active=true
        └── GtkButton "Save" [w12] sensitive=false
```

Rules:

- One node per line; indentation and box characters encode hierarchy.
- `[wN]` is always present and is the handle used by every other tool.
- Quoted text is escaped and cut at `text_limit`, marked with `…`.
- State words appear only when they deviate from the parent's or are
  diagnostically relevant: `mapped=false`, `sensitive=false`, `detached`,
  `native`, `secret`; `expanded` is shown whenever the widget is expandable.

## References

- References are opaque (`w42`), never memory addresses, child indices, or
  screen coordinates.
- Ref ids are stored in the object's qdata, and the registry holds only weak
  references. Taking a snapshot never keeps a widget alive, and a reused
  memory address never inherits an old ref.
- Within one session, the same object keeps the same reference across
  snapshots, so two snapshots can be compared.
- Object states: `live` (inside a root), `detached` (alive but without a
  root, and not itself a root), `stale` (finalized). Operations that need a
  root fail `detached`; `inspect` and `get_property` still work. Resolving a
  finalized object returns `stale_reference`.
- Non-widget GObjects returned by other tools (models, list items, buffers,
  controllers, object-valued properties) get `oN` references with the same
  rules. The prefix is informational; tools validate the object's type.
- Actions are names, not objects; they are addressed as `{scope, name}`.
- References are not portable across sessions, processes, or broker restarts.

## Semantics and limits

- **Point in time.** A snapshot is built on the GTK main thread and reflects
  one moment. Call `snapshot` again, or use `watch`, to observe change.
- **Existing children only.** In a virtualized `GtkListView`, only row widgets
  that exist appear as children (`children_source: "realized"`); the item set
  is available through `inspect_model`.
- **Unmapped content is included by default.** Hidden stack pages, unmapped
  widgets, and detached content are listed so agents can find them before
  revealing them. `capture` refuses unmapped targets.
- **Truncation is explicit.** When `max_nodes` or `max_depth` cuts the tree,
  the response sets `truncated: true`, reports `node_count` for the nodes
  returned, and marks each cut node with `children_truncated: true` plus the
  omitted count when known. `max_nodes` is one budget for all roots: roots
  that no longer fit are left out and counted in `omitted_roots`.
- **Ordering is GTK child order**, followed by detached content.

## Text and redaction

Secrecy follows `plan.md` 4.4:

- Secret nodes report `"text_redacted": true` and omit content and length,
  in both formats.
- Secret objects never match `text` / `text_contains` selectors.
- Other text is cut at `text_limit` with `"text_truncated": true`.
- Text read failures produce a node warning, not a snapshot failure.

## Comparing snapshots

The broker retains recent snapshots per session (`--retain-snapshots`) and
offers:

```json
{ "tool": "diff_snapshots", "session": "s1", "from": "snap-1041", "to": "snap-1042" }
```

The diff reports, per widget ref: added nodes, removed nodes, and changed
nodes with the fields that changed (`state.sensitive`, `text`, `attached`,
child count, allocation size). It compares snapshot data only and does not
claim why a change happened. Evicted snapshots return `snapshot_unavailable`.

## Worked flow

```text
snapshot(session, view="semantic")
  → GtkExpander "Advanced options" [w8] expanded=false
    └── GtkEntry [w9] detached via=GtkExpander:child

interact(session, target="w8", operation="set_expanded", expanded=true)
  → {"observed": {"expanded": true}}

wait_for(session, target={"automation_id": "server-address"}, condition="mapped")
  → satisfied, [w9]

capture(session, target="w8")
  → PNG of the expanded expander, stale=false
```

## Acceptance checks

These are covered by `plan.md` AC-2, AC-3, and AC-6:

- A collapsed expander's child is listed with `attached: false` and
  `via: "GtkExpander:child"`; after `set_expanded`, a second snapshot shows it
  attached and mapped, with the same ref.
- A widget removed from its parent but kept alive returns `detached` from
  `interact`; once finalized, its ref returns `stale_reference`; the agent
  never keeps either alive.
- A list view with more model items than row widgets reports
  `children_source: "realized"`.
- A closed popover appears once, in place, with `native: true` and
  `children_collapsed: true`.
- Exceeding `max_nodes` sets `truncated: true` and marks cut nodes.
- A password entry never exposes its text or length in either format.
- Text output and JSON output of the same snapshot describe the same
  hierarchy and references.
