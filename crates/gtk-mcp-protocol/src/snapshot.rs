//! Text rendering of the canonical snapshot JSON (`docs/widget-tree.md`).
//! The agent produces only JSON; text is derived from the same data so both
//! formats describe the same hierarchy and refs.

use serde_json::Value;
use std::fmt::Write;

/// Render a snapshot document (`{"roots": [...], ...}`) as text.
pub fn render_text(snapshot: &Value, text_limit: usize) -> String {
    let mut out = String::new();
    if let Some(roots) = snapshot.get("roots").and_then(Value::as_array) {
        for root in roots {
            node_line(&mut out, "", "", root, None, text_limit);
        }
    }
    if snapshot.get("truncated").and_then(Value::as_bool) == Some(true) {
        let _ = writeln!(
            out,
            "… truncated at {} nodes",
            snapshot.get("node_count").and_then(Value::as_u64).unwrap_or(0)
        );
    }
    out
}

fn quote(s: &str, limit: usize) -> String {
    let mut out = String::from("\"");
    let mut n = 0;
    for ch in s.chars() {
        if n >= limit {
            out.push('…');
            break;
        }
        n += 1;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn flag(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}

fn state_flag(node: &Value, k: &str) -> Option<bool> {
    node.get("state").and_then(|s| s.get(k)).and_then(Value::as_bool)
}

fn num(v: &Value) -> String {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { format!("{f}") }
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn node_line(out: &mut String, prefix: &str, connector: &str, node: &Value, parent: Option<&Value>, limit: usize) {
    let ty = str_of(node, "type").unwrap_or("?");
    let mut line = String::from(ty);
    if let Some(l) = str_of(node, "label").or_else(|| str_of(node, "title")) {
        let _ = write!(line, " {}", quote(l, limit));
    }
    let _ = write!(line, " [{}]", str_of(node, "ref").unwrap_or("?"));
    if parent.is_none() {
        if let Some(b) = node.get("bounds_in_root") {
            if let (Some(w), Some(h)) = (b.get("width"), b.get("height")) {
                let _ = write!(line, " {}x{}", num(w), num(h));
            }
        }
        if state_flag(node, "mapped") == Some(true) {
            line.push_str(" mapped");
        }
    }
    if flag(node, "secret") == Some(true) {
        line.push_str(" secret");
    }
    if flag(node, "native") == Some(true) {
        line.push_str(" native");
        if flag(node, "children_collapsed") == Some(true) {
            let n = node.get("child_count").and_then(Value::as_u64).unwrap_or(0);
            let _ = write!(line, " closed ({n} children)");
        }
    }
    if flag(node, "attached") == Some(false) {
        line.push_str(" detached");
        if let Some(via) = str_of(node, "via") {
            let _ = write!(line, " via={via}");
        }
    }
    if parent.is_some() {
        for k in ["mapped", "sensitive"] {
            if state_flag(node, k) == Some(false) {
                let pv = parent.and_then(|p| state_flag(p, k));
                if pv != Some(false) {
                    let _ = write!(line, " {k}=false");
                }
            }
        }
    } else if state_flag(node, "sensitive") == Some(false) {
        line.push_str(" sensitive=false");
    }
    for k in ["expanded", "active", "checked", "selected"] {
        if let Some(b) = state_flag(node, k) {
            let _ = write!(line, " {k}={b}");
        }
    }
    if let Some(v) = node.get("value").and_then(|v| v.get("current")) {
        let _ = write!(line, " value={}", num(v));
    }
    if flag(node, "text_redacted") == Some(true) {
        line.push_str(" text=<redacted>");
    } else if let Some(t) = str_of(node, "text") {
        let _ = write!(line, " text={}", quote(t, limit));
    }
    if str_of(node, "children_source") == Some("realized") {
        line.push_str(" children=realized");
    }
    let _ = writeln!(out, "{prefix}{connector}{line}");

    let children: &[Value] = node.get("children").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let elided = node.get("elided_children").and_then(Value::as_u64).unwrap_or(0);
    let omitted = if flag(node, "children_truncated") == Some(true) {
        Some(node.get("omitted_children").and_then(Value::as_u64))
    } else {
        None
    };
    let child_prefix = match connector {
        "" => prefix.to_string(),
        "└── " => format!("{prefix}    "),
        _ => format!("{prefix}│   "),
    };
    let tail = usize::from(elided > 0) + usize::from(omitted.is_some());
    let total = children.len() + tail;
    for (i, c) in children.iter().enumerate() {
        let last = i + 1 == total;
        node_line(out, &child_prefix, if last { "└── " } else { "├── " }, c, Some(node), limit);
    }
    let mut idx = children.len();
    if elided > 0 {
        idx += 1;
        let conn = if idx == total { "└── " } else { "├── " };
        let _ = writeln!(out, "{child_prefix}{conn}… {elided} structural widgets");
    }
    if let Some(n) = omitted {
        let conn = "└── ";
        match n {
            Some(n) => {
                let _ = writeln!(out, "{child_prefix}{conn}… {n} more (truncated)");
            }
            None => {
                let _ = writeln!(out, "{child_prefix}{conn}… more (truncated)");
            }
        }
    }
}
