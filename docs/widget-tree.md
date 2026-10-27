# Widget tree snapshot

## Purpose

The widget tree is the main thing an agent looks at in `gtk-mcp`. It shows
what a GTK app contains right now. The agent picks a widget from it, then
calls `inspect`, `interact`, `inspect_layout` or `capture` with a reference
that points at exactly that widget.

It is the GTK widget tree. It is not the accessibility tree, and it is not a
tree of Wayland surfaces:

- The full view includes GTK's internal containers and widgets you can't
  act on.
- Event controllers, actions, models and other non-widget objects are not
  children in the tree. You reach them through `inspect` on the widget that
  owns them.
- In GTK 4 a popover is a child of the widget that owns it, so it appears in
  that spot, marked `native: true`. It is not repeated as a separate root.
  `list_windows` lists open popovers and other native surfaces separately.
- Some widgets keep content that is not currently in the tree, such as the
  child of a collapsed `GtkExpander`. That content is still listed, marked
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
| `session` | The app session to read. Required. |
| `root` | A target (`plan.md` section 7): a ref, or a selector whose `window` key uses `WindowSel`. If left out, all top-level windows in the session. |
| `view` | `full` or `semantic`. Default: `full`. |
| `format` | `text` or `json`. Default: `text`. JSON is the reference form; text is made from the same data. |
| `max_depth` | How many levels below the root to include. |
| `max_nodes` | The most nodes to return. Cut-offs are always reported. |
| `include_unmapped` | Include widgets that exist but are not on screen (not mapped). Default: `true`. |
| `include_closed_popovers` | Show what is inside closed popovers. Default: `false`: a closed popover is one node with `children_collapsed: true` and its child count. |
| `text_limit` | The most characters of label or text per node. Cut-offs are reported. |

### Views

The `full` view lists every widget you reach through
`gtk_widget_get_first_child` / `gtk_widget_get_next_sibling`, in child order,
plus detached content, up to the limits you set.

The `semantic` view is shorter, for getting a task done. It keeps widgets
you can act on, widgets with text, selectable widgets and widgets with
state, plus the parents needed to keep the hierarchy. It may hide widgets
that only do layout or decoration. Hidden widgets are marked in JSON
(`elided_children`) and in text (`… 3 structural widgets`). The semantic
view never makes up nodes and never moves the nodes it keeps: a kept widget
stays under its nearest kept ancestor, and the marker shows that levels were
skipped. A node cut off by `max_depth` or `max_nodes` is always kept, so its
`children_truncated` marker stays visible.

The default is `full`. This is a tool for GTK development, and layout bugs
usually sit in the layout widgets that the semantic view would hide.

### Detached content

Some widgets remove their content from the tree while it is hidden but keep
a reference to it. The agent lists that content under the widget that holds
it, using a small adapter per widget type that reads public properties:

| Holder | Source | Note |
|---|---|---|
| `GtkExpander` | `child` property | Unparented while collapsed |

These nodes carry `attached: false` and `via: "GtkExpander:child"`. Their
children are listed as usual. New adapters are added only for GTK behavior
that has been checked.

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

Every node has:

| Field | Meaning |
|---|---|
| `ref` | A reference to this widget within the session, e.g. `w42`. See "References". |
| `type` | GType name, e.g. `GtkExpander`, `AdwExpanderRow`, `GtkListView`. |
| `children` | Child nodes in GTK child order, then detached content. An empty array if there are none. |

These appear when GTK provides the information:

| Field | Meaning |
|---|---|
| `identity.buildable_id` | The `GtkBuildable` ID, if set. On GTK ≥ 4.22 this is also the widget's AT-SPI `AccessibleId`. |
| `identity.automation_id` | The widget name, if the app set one (`gtk_widget_set_name`). GTK returns the type name when no name is set; that case is left out. The app doesn't need to depend on the agent for this. See `embedding.md`. |
| `identity.css_name` | What `gtk_widget_get_css_name` returns. |
| `identity.css_classes` | What `gtk_widget_get_css_classes` returns, minus the agent's own classes (`gtk-mcp-highlight`). |
| `role` | The accessible role from `gtk_accessible_get_accessible_role`. It is extra information about the node; the tree is still the widget tree. |
| `a11y.name` | The accessible name. GTK has no public way to read accessible properties from inside the app, so this only appears when the broker's AT-SPI source is on and the node could be linked. It is marked `"source": "atspi"` and kept separate from the widget's text. |
| `label` | A readable label (button label, expander label, row title), when an adapter can read it. |
| `text` | Text content of text widgets. See "Text and redaction". |
| `value` | For range-like widgets: `{"current": …, "min": …, "max": …}`. |
| `state` | The flags that apply: `visible`, `mapped`, `sensitive`, `focusable`, `has_focus`, `expanded`, `active`, `checked`, `selected`. Flags that don't apply are left out instead of being reported as `false`. |
| `native` | `true` for popovers and other native surfaces inside the tree. |
| `attached` | `false` for detached content, otherwise left out. `via` names the adapter. |
| `children_collapsed` | A closed popover whose contents were not listed. Comes with `child_count`. |
| `children_source` | `"realized"` on list views: the children are the row widgets that exist, not the model items. |
| `allocation` | `{"width": …, "height": …}` from the current allocation. |
| `bounds_in_root` | The rectangle relative to the snapshot root, from `gtk_widget_compute_bounds`. It works across popovers. If it can't be computed, it is left out and the node gets a warning. |
| `capabilities` | The operations this node supports, e.g. `["set_expanded", "activate", "focus"]`. `interact` uses this list. |
| `elided_children` | Semantic view only: how many nodes below this one were hidden. |
| `secret` | `true` when the node is in the secret set (`plan.md` 4.4). |
| `warnings` | Problems with this node, e.g. bounds unavailable or text unreadable. |

The agent doesn't guess at data it doesn't know or that depends on the
version. Extra data goes under its own key (`adw`, `a11y`), announced in the
handshake capabilities. The core fields never change to carry it.

### Text format

The text format shows the same tree as the JSON, with some detail left out.
It keeps the hierarchy, order, type, reference, important state and cut-down
text. Long strings and class lists may be shortened.

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

- One node per line. Indentation and box-drawing characters show the
  hierarchy.
- `[wN]` is always there. It is the handle every other tool uses.
- Quoted text is escaped and cut at `text_limit`, marked with `…`.
- State words appear only when they differ from the parent or help with
  debugging: `mapped=false`, `sensitive=false`, `detached`, `native`,
  `secret`. `expanded` is always shown on widgets that can expand.

## References

- References are opaque labels like `w42`. They are never memory addresses,
  child positions or screen coordinates.
- The ref id is stored on the object itself (in its qdata), and the agent's
  registry holds only weak references. Taking a snapshot never keeps a widget
  alive, and a new object at a reused memory address never gets an old ref.
- Within one session, an object keeps the same reference from one snapshot
  to the next, so you can compare snapshots.
- An object is `live` (inside a window or other root), `detached` (alive but
  not inside a root, and not a root itself) or `stale` (destroyed).
  Operations that need a root fail with `detached`; `inspect` and
  `get_property` still work. A ref to a destroyed object returns
  `stale_reference`.
- Other GObjects that tools return (models, list items, buffers,
  controllers, object-valued properties) get `oN` references with the same
  rules. The prefix is only a hint; tools check the object's actual type.
- Actions are names, not objects. You address them as `{scope, name}`.
- References only work in their own session. They don't carry over to other
  sessions, processes or a restarted broker.

## Semantics and limits

- A snapshot shows one moment. The agent builds it on the GTK main thread.
  To see changes, take another `snapshot` or use `watch`.
- Only children that exist are listed. A `GtkListView` only creates widgets
  for some rows, so its children are those row widgets
  (`children_source: "realized"`). Use `inspect_model` for the full list of
  items.
- Content that is not on screen is included by default: hidden stack pages,
  unmapped widgets and detached content. That way the agent can find them
  before showing them. `capture` refuses targets that are not on screen.
- Cut-offs are always reported. When `max_nodes` or `max_depth` cuts the
  tree, the response sets `truncated: true` and gives `node_count` for the
  nodes it returned. Each cut node gets `children_truncated: true`, plus how
  many children were left out when that is known. `max_nodes` is shared by
  all roots: roots that no longer fit are left out and counted in
  `omitted_roots`.
- Nodes come in GTK child order, followed by detached content.

## Text and redaction

Secrets follow `plan.md` 4.4:

- Secret nodes report `"text_redacted": true` and leave out both the text
  and its length, in both formats.
- Secret objects never match `text` / `text_contains` selectors.
- Other text is cut at `text_limit` and marked `"text_truncated": true`.
- If text can't be read, the node gets a warning; the snapshot still works.

## Comparing snapshots

The broker keeps recent snapshots for each session (`--retain-snapshots`)
and can compare two of them:

```json
{ "tool": "diff_snapshots", "session": "s1", "from": "snap-1041", "to": "snap-1042" }
```

For each widget ref, the diff lists added nodes, removed nodes, and changed
nodes with the fields that changed (`state.sensitive`, `text`, `attached`,
child count, allocation size). It only compares the snapshot data and says
nothing about why something changed. A snapshot that is no longer kept
returns `snapshot_unavailable`.

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

## Guarantees

These are tested (`plan.md` AC-2, AC-3 and AC-6):

- A collapsed expander's child is listed with `attached: false` and
  `via: "GtkExpander:child"`. After `set_expanded`, a second snapshot shows
  it attached and mapped, with the same ref.
- A widget removed from its parent but still alive returns `detached` from
  `interact`. Once it is destroyed, its ref returns `stale_reference`. The
  agent never keeps either one alive.
- A list view with more model items than row widgets reports
  `children_source: "realized"`.
- A closed popover appears once, in its place, with `native: true` and
  `children_collapsed: true`.
- Going over `max_nodes` sets `truncated: true` and marks the cut nodes.
- A password entry never shows its text or its length, in either format.
- The text and JSON output of the same snapshot describe the same hierarchy
  and references.
