---
name: gtk-mcp
description: Use when inspecting, testing or debugging a running GTK4 or libadwaita app from the shell with the gtk-mcp CLI - reading the widget tree, finding widgets, clicking buttons, filling entries, waiting for UI state, checking layout, CSS or accessibility, and capturing widget PNGs. Trigger when the user asks to look at, poke, verify or screenshot their GTK app's UI.
---

# gtk-mcp

Drive a running GTK4/libadwaita app from inside its process through the `gtk-mcp` CLI.

## First step

Run:

```sh
gtk-mcp prime
```

It prints the full usage guide and tool index. Follow it.

If `gtk-mcp` is not installed:

```sh
curl -fsSL https://raw.githubusercontent.com/alex-oleshkevich/gtk-mcp/main/install.sh | sh
```

The app must be built with the gtk-mcp agent attached (see the project README, "Add it to your
app"). `gtk-mcp apps` lists the apps it can reach; `gtk-mcp doctor` explains why one is missing.

## Rules

- Look before acting: `snapshot` or `query` first, then `interact`, then read the `observed` result.
- Check arguments with `gtk-mcp tools NAME` instead of guessing.
- Each `gtk-mcp call` is its own connection. Put steps that must share state (temporary CSS, a
  capture after a change) in one `batch`.
- Save captures with `-o FILE.png` and view the file to verify visual changes.
