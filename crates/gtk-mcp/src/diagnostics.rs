//! `read_events` and `read_logs`: answered from the session's ring buffers
//! without calling the agent, so they also work after the app exited
//! (`docs/plan.md` 5.5, 8; `docs/tools.md` diagnostics).

use crate::broker::ToolOutcome;
use crate::session::Session;
use gtk_mcp_protocol::{
    AgentError, DEFAULT_READ_LIMIT, ErrorCode, LogSeverity, MAX_READ_LIMIT, ReadEventsParams, ReadLogsParams,
};
use serde_json::{Value, json};

fn invalid(msg: impl Into<String>) -> AgentError {
    AgentError::invalid_params(msg)
}

fn limit_of(requested: Option<u32>) -> Result<usize, AgentError> {
    let l = requested.unwrap_or(DEFAULT_READ_LIMIT);
    if l == 0 || l > MAX_READ_LIMIT {
        return Err(invalid(format!("limit must be in 1..={MAX_READ_LIMIT}")));
    }
    Ok(l as usize)
}

/// Shared paging: filter, then cut at `limit`. `next_cursor` continues right
/// after the last entry returned when the cut dropped matches, otherwise after
/// everything read.
fn page(read: &crate::ring::Read, limit: usize, keep: impl Fn(&Value) -> bool, map: impl Fn(&Value) -> Value) -> Value {
    let mut out = Vec::new();
    let mut next = read.next_cursor;
    for e in &read.entries {
        if !keep(&e.value) {
            continue;
        }
        if out.len() == limit {
            next = e.cursor;
            break;
        }
        let mut v = map(&e.value);
        if let Value::Object(o) = &mut v {
            o.insert("cursor".into(), json!(e.cursor));
        }
        out.push(v);
    }
    json!({
        "entries": out,
        "next_cursor": next,
        "more": next < read.next_cursor,
        "dropped": read.dropped,
        "dropped_total": read.dropped_total,
    })
}

pub fn read_events(session: &Session, p: ReadEventsParams) -> Result<ToolOutcome, AgentError> {
    let limit = limit_of(p.limit)?;
    let read = session.events_since(p.since.unwrap_or(0));
    let keep = |v: &Value| {
        p.subscription.as_deref().is_none_or(|s| v["subscription"] == s) && p.kind.as_deref().is_none_or(|k| v["kind"] == k)
    };
    let mut r = page(&read, limit, keep, |v| match v {
        Value::Object(_) => v.clone(),
        other => json!({"value": other}),
    });
    r["session"] = json!(session.id());
    Ok(ToolOutcome::json(&r))
}

/// Severity, domain and message of one GLib default-writer line, e.g.
/// `(app:1234): Gtk-CRITICAL **: 12:00:00.123: gtk_widget_foo: assertion failed`
/// or `Gtk-Message: 12:00:00.123: text`. `None` when the line is not a GLib record.
pub fn parse_glib_line(line: &str) -> Option<(LogSeverity, String, String)> {
    let mut rest = line.trim_start();
    if let Some(r) = rest.strip_prefix("** ") {
        rest = r;
    }
    // "(program:pid): " prefix
    if rest.starts_with('(') {
        let end = rest.find("): ")?;
        rest = &rest[end + 3..];
    }
    // `Domain-LEVEL **: ...` or `Domain-Message: ...` or legacy `LEVEL **: ...`
    let (head, tail) = match rest.find(" **: ") {
        Some(i) => (&rest[..i], &rest[i + 5..]),
        None => {
            let i = rest.find(": ")?;
            (&rest[..i], &rest[i + 2..])
        }
    };
    if head.is_empty() || head.contains(' ') {
        return None;
    }
    let (domain, level) = match head.rsplit_once('-') {
        Some((d, l)) => (d, l),
        None => ("", head),
    };
    let severity = LogSeverity::parse(level)?;
    // optional "HH:MM:SS.mmm: " timestamp
    let msg = match tail.get(..14) {
        Some(t) if t.as_bytes()[2] == b':' && t.as_bytes()[5] == b':' && t.as_bytes()[8] == b'.' && t.ends_with(": ") => &tail[14..],
        _ => tail,
    };
    Some((severity, domain.to_owned(), msg.to_owned()))
}

pub fn read_logs(session: &Session, p: ReadLogsParams) -> Result<ToolOutcome, AgentError> {
    let limit = limit_of(p.limit)?;
    let forwarded = session.log_forwarding();
    if !session.launched_by_broker() && !forwarded {
        return Err(AgentError::new(
            ErrorCode::Unavailable,
            "logs are only captured for apps this broker launched (launch_app) and for apps that forward records with gtk_mcp_agent::forward_log",
        )
        .with_data(json!({"launched_by_broker": false, "log_forwarding": false})));
    }
    if let Some(s) = &p.source {
        if !matches!(s.as_str(), "stdout" | "stderr" | "forwarded") {
            return Err(invalid("source must be `stdout`, `stderr` or `forwarded`"));
        }
    }
    let read = session.logs_since(p.since.unwrap_or(0));
    // Parse once per record; filters and output share the result.
    let parse = |v: &Value| -> Value {
        let mut o = v.as_object().cloned().unwrap_or_default();
        if !o.contains_key("level") {
            if let Some((sev, domain, msg)) = v["text"].as_str().and_then(parse_glib_line) {
                o.insert("level".into(), json!(sev.as_str()));
                o.insert("domain".into(), json!(domain));
                o.insert("message".into(), json!(msg));
            }
        }
        Value::Object(o)
    };
    let keep = |v: &Value| {
        let rec = parse(v);
        if p.source.as_deref().is_some_and(|s| rec["source"] != s) {
            return false;
        }
        if let Some(min) = p.min_level {
            match rec["level"].as_str().and_then(LogSeverity::parse) {
                Some(l) if l >= min => {}
                _ => return false,
            }
        }
        if let Some(c) = &p.contains {
            let hay = rec["message"].as_str().or_else(|| rec["text"].as_str()).unwrap_or_default();
            if !hay.contains(c.as_str()) {
                return false;
            }
        }
        true
    };
    let mut r = page(&read, limit, keep, parse);
    r["session"] = json!(session.id());
    r["sources"] = json!({"captured_output": session.launched_by_broker(), "forwarded": forwarded});
    Ok(ToolOutcome::json(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_writer_lines() {
        let (l, d, m) = parse_glib_line("(fixture:12): Gtk-CRITICAL **: 12:00:00.123: gtk_foo: assertion 'x' failed").unwrap();
        assert_eq!((l, d.as_str(), m.as_str()), (LogSeverity::Critical, "Gtk", "gtk_foo: assertion 'x' failed"));
        let (l, d, m) = parse_glib_line("Gtk-Message: 16:34:28.205: Failed to load module").unwrap();
        assert_eq!((l, d.as_str(), m.as_str()), (LogSeverity::Message, "Gtk", "Failed to load module"));
        let (l, d, _) = parse_glib_line("** (app:1): WARNING **: 12:00:00.000: odd").unwrap();
        assert_eq!((l, d.as_str()), (LogSeverity::Warning, ""));
        let (l, d, _) = parse_glib_line("GLib-GIO-WARNING **: 12:00:00.000: x").unwrap();
        assert_eq!((l, d.as_str()), (LogSeverity::Warning, "GLib-GIO"));
        assert!(parse_glib_line("fixture-banner: hello").is_none());
        assert!(parse_glib_line("plain text").is_none());
    }
}
