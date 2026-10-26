//! `inspect_text` / `edit_text`: text buffers, cursor, selection and tag spans
//! of `GtkTextView` / `GtkTextBuffer` and any `GtkEditable`. Offsets are
//! character offsets. Secret objects (plan 4.4) are never read: `inspect_text`
//! fails `redacted` and `edit_text` reports `{"text": {"redacted": true}}`,
//! neither revealing text, length, cursor or selection.

use super::observe::finish;
use crate::agent::{Agent, Ctx};
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, EditTextOp, EditTextParams, ErrorCode, InspectTextParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;

const DEFAULT_LIMIT: u32 = 4096;
const ECHO_LIMIT: usize = 4096;
const MAX_RANGES_PER_TAG: usize = 1000;

fn invalid(msg: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::InvalidValue, msg)
}

fn unsupported(msg: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::UnsupportedOperation, msg)
}

enum Src {
    Buffer { buf: gtk::TextBuffer, view: Option<gtk::TextView> },
    Editable { w: gtk::Widget },
}

struct Resolved {
    src: Src,
    /// The widget to observe / check sensitivity of; `None` for a bare buffer.
    widget: Option<gtk::Widget>,
    target: String,
    secret: bool,
}

fn redacted_error(target: &str) -> AgentError {
    AgentError::new(ErrorCode::Redacted, format!("{target} is a secret text object; its content is never returned"))
}

/// A bare buffer is secret when it was marked so, or when any text view
/// showing it is secret.
fn buffer_is_secret(buf: &gtk::TextBuffer) -> bool {
    if secrecy::is_secret(buf) {
        return true;
    }
    fn walk(w: &gtk::Widget, buf: &gtk::TextBuffer) -> bool {
        if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
            if &tv.buffer() == buf && secrecy::is_secret(w) {
                return true;
            }
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if walk(&child, buf) {
                return true;
            }
            c = child.next_sibling();
        }
        false
    }
    selector::toplevels().iter().any(|t| walk(t.upcast_ref(), buf))
}

fn resolve(agent: &Agent, target: &gtk_mcp_protocol::Target) -> Result<Resolved, AgentError> {
    let obj = selector::resolve_object(agent, target)?;
    let r = agent.registry.ref_for(&obj);
    if let Some(w) = obj.downcast_ref::<gtk::Widget>() {
        if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
            let buf = tv.buffer();
            secrecy::inherit(tv.upcast_ref(), buf.upcast_ref());
            let secret = secrecy::is_secret(w) || secrecy::is_secret(&buf);
            return Ok(Resolved {
                src: Src::Buffer { buf, view: Some(tv.clone()) },
                widget: Some(w.clone()),
                target: r,
                secret,
            });
        }
        if let Some(ed) = w.dynamic_cast_ref::<gtk::Editable>() {
            let delegate_secret = ed.delegate().is_some_and(|d| secrecy::is_secret(&d));
            let secret = secrecy::is_secret(w) || delegate_secret;
            return Ok(Resolved { src: Src::Editable { w: w.clone() }, widget: Some(w.clone()), target: r, secret });
        }
        return Err(unsupported(format!("{} is neither a text view nor an editable", widgets::type_name(w))));
    }
    if let Ok(buf) = obj.clone().downcast::<gtk::TextBuffer>() {
        let secret = buffer_is_secret(&buf);
        return Ok(Resolved { src: Src::Buffer { buf, view: None }, widget: None, target: r, secret });
    }
    if secrecy::is_secret(&obj) {
        return Err(redacted_error(&r));
    }
    Err(unsupported(format!("{} is not a text buffer, text view or editable", obj.type_().name())))
}

fn editable_of(w: &gtk::Widget) -> &gtk::Editable {
    w.dynamic_cast_ref::<gtk::Editable>().expect("resolved as editable")
}

fn char_slice(s: &str, start: usize, end: usize) -> String {
    s.chars().skip(start).take(end.saturating_sub(start)).collect()
}

fn check_range(start: i32, end: i32, len: usize, secret: bool) -> Result<(usize, usize), AgentError> {
    let ok = start >= 0 && end >= start && (end as usize) <= len;
    if !ok {
        // A secret's length must not leak through the message.
        let msg = if secret {
            "the range is outside the text".to_string()
        } else {
            format!("range [{start}, {end}) is invalid for a text of {len} characters")
        };
        return Err(invalid(msg).with_data(if secret { Value::Null } else { json!({"length": len}) }));
    }
    Ok((start as usize, end as usize))
}

// -- inspect_text -----------------------------------------------------------------

pub fn inspect_text(agent: &Rc<Agent>, p: InspectTextParams) -> Result<Value, AgentError> {
    let res = resolve(agent, &p.target)?;
    if res.secret {
        return Err(redacted_error(&res.target));
    }
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT) as usize;
    let mut m = Map::new();
    m.insert("target".into(), json!(res.target));
    match &res.src {
        Src::Buffer { buf, view } => {
            let len = buf.char_count() as usize;
            let (start, end) = check_range(p.start.unwrap_or(0), p.end.unwrap_or(len as i32), len, false)?;
            let ret_end = end.min(start.saturating_add(limit));
            let text = buf.slice(&buf.iter_at_offset(start as i32), &buf.iter_at_offset(ret_end as i32), true);
            m.insert("kind".into(), json!("text_view"));
            m.insert("length".into(), json!(len));
            m.insert("line_count".into(), json!(buf.line_count()));
            m.insert("text".into(), json!(text.as_str()));
            m.insert("range".into(), json!({"start": start, "end": ret_end}));
            m.insert("truncated".into(), json!(ret_end < end));
            let cursor = buf.cursor_position();
            m.insert("cursor".into(), json!({"offset": cursor, "line": buf.iter_at_offset(cursor).line()}));
            m.insert(
                "selection".into(),
                match buf.selection_bounds() {
                    Some((s, e)) => json!({"start": s.offset(), "end": e.offset()}),
                    None => Value::Null,
                },
            );
            if let Some(v) = view {
                m.insert("editable".into(), json!(v.is_editable()));
            }
            if p.include_tags == Some(true) {
                m.insert("tags".into(), tags_json(buf, start, ret_end));
            }
        }
        Src::Editable { w } => {
            let ed = editable_of(w);
            let full = ed.text();
            let len = full.chars().count();
            let (start, end) = check_range(p.start.unwrap_or(0), p.end.unwrap_or(len as i32), len, false)?;
            let ret_end = end.min(start.saturating_add(limit));
            m.insert("kind".into(), json!("editable"));
            m.insert("length".into(), json!(len));
            m.insert("text".into(), json!(char_slice(&full, start, ret_end)));
            m.insert("range".into(), json!({"start": start, "end": ret_end}));
            m.insert("truncated".into(), json!(ret_end < end));
            m.insert("cursor".into(), json!({"offset": ed.position()}));
            m.insert(
                "selection".into(),
                match ed.selection_bounds() {
                    Some((s, e)) => json!({"start": s, "end": e}),
                    None => Value::Null,
                },
            );
            m.insert("editable".into(), json!(ed.is_editable()));
        }
    }
    Ok(Value::Object(m))
}

/// Spans of every named tag intersecting `[start, end)`.
fn tags_json(buf: &gtk::TextBuffer, start: usize, end: usize) -> Value {
    let mut tags: Vec<gtk::TextTag> = Vec::new();
    buf.tag_table().foreach(|t| tags.push(t.clone()));
    let total = buf.char_count();
    let mut out: Vec<(String, Vec<[i32; 2]>)> = Vec::new();
    for tag in tags {
        let Some(name) = tag.name() else { continue };
        let mut ranges: Vec<[i32; 2]> = Vec::new();
        let mut iter = buf.start_iter();
        let mut open: Option<i32> = if iter.starts_tag(Some(&tag)) { Some(0) } else { None };
        while ranges.len() < MAX_RANGES_PER_TAG && iter.forward_to_tag_toggle(Some(&tag)) {
            if iter.starts_tag(Some(&tag)) {
                open = Some(iter.offset());
            } else if iter.ends_tag(Some(&tag)) {
                if let Some(s) = open.take() {
                    ranges.push([s, iter.offset()]);
                }
            }
        }
        if let Some(s) = open {
            ranges.push([s, total]);
        }
        ranges.retain(|r| (r[0] as usize) < end && (r[1] as usize) > start);
        if !ranges.is_empty() {
            out.push((name.to_string(), ranges));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Array(out.into_iter().map(|(name, ranges)| json!({"name": name, "ranges": ranges})).collect())
}

// -- edit_text --------------------------------------------------------------------

fn observed_state(res: &Resolved) -> Value {
    if res.secret {
        return json!({"text": secrecy::redacted()});
    }
    let (text, len, cursor, selection) = match &res.src {
        Src::Buffer { buf, .. } => {
            let (s, e) = buf.bounds();
            let cur = buf.cursor_position();
            (
                buf.slice(&s, &e, true).to_string(),
                buf.char_count() as usize,
                json!({"offset": cur, "line": buf.iter_at_offset(cur).line()}),
                match buf.selection_bounds() {
                    Some((a, b)) => json!({"start": a.offset(), "end": b.offset()}),
                    None => Value::Null,
                },
            )
        }
        Src::Editable { w } => {
            let ed = editable_of(w);
            let t = ed.text().to_string();
            let n = t.chars().count();
            (
                t,
                n,
                json!({"offset": ed.position()}),
                match ed.selection_bounds() {
                    Some((a, b)) => json!({"start": a, "end": b}),
                    None => Value::Null,
                },
            )
        }
    };
    let mut m = Map::new();
    if len > ECHO_LIMIT {
        m.insert("text".into(), json!(text.chars().take(ECHO_LIMIT).collect::<String>()));
        m.insert("truncated".into(), json!(true));
    } else {
        m.insert("text".into(), json!(text));
    }
    m.insert("length".into(), json!(len));
    m.insert("cursor".into(), cursor);
    m.insert("selection".into(), selection);
    Value::Object(m)
}

fn text_len(res: &Resolved) -> usize {
    match &res.src {
        Src::Buffer { buf, .. } => buf.char_count() as usize,
        Src::Editable { w } => editable_of(w).text().chars().count(),
    }
}

pub async fn edit_text(agent: &Rc<Agent>, p: EditTextParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let res = resolve(agent, &p.target)?;
    if let Some(w) = &res.widget {
        if !w.is_sensitive() {
            return Err(unsupported(format!("{} is insensitive", res.target)).with_data(json!({"reason": "insensitive"})));
        }
    }
    let modifies = !matches!(p.op, EditTextOp::SetSelection { .. });
    if modifies {
        let editable = match &res.src {
            Src::Buffer { view, .. } => view.as_ref().is_none_or(|v| v.is_editable()),
            Src::Editable { w } => editable_of(w).is_editable(),
        };
        if !editable {
            return Err(unsupported(format!("{} is read-only", res.target)));
        }
    }
    // A secret's length is never revealed, not even through bounds errors:
    // only the public shape of a range is validated and GTK clamps the rest.
    let secret = res.secret;
    let len = if secret { i32::MAX as usize } else { text_len(&res) };
    match &p.op {
        EditTextOp::Insert { offset, text } => {
            let (at, _) = check_range(*offset, *offset, len, secret)?;
            insert(&res.src, at as i32, text);
        }
        EditTextOp::Delete { start, end } => {
            let (s, e) = check_range(*start, *end, len, secret)?;
            delete(&res.src, s as i32, e as i32);
        }
        EditTextOp::Replace { start, end, text } => {
            let (s, e) = check_range(*start, *end, len, secret)?;
            replace(&res.src, s as i32, e as i32, text);
        }
        EditTextOp::SetSelection { start, end } => {
            // Either end may be the cursor; both must lie inside the text.
            check_range(*start.min(end), *start.max(end), len, secret)?;
            match &res.src {
                Src::Buffer { buf, .. } => buf.select_range(&buf.iter_at_offset(*end), &buf.iter_at_offset(*start)),
                Src::Editable { w } => editable_of(w).select_region(*start, *end),
            }
        }
    }
    let observed = observed_state(&res);
    Ok(finish(agent, res.target.clone(), observed, res.widget.as_ref(), p.observe.as_ref(), ctx).await)
}

fn insert(src: &Src, at: i32, text: &str) {
    match src {
        Src::Buffer { buf, .. } => {
            let mut it = buf.iter_at_offset(at);
            buf.insert(&mut it, text);
        }
        Src::Editable { w } => {
            let mut pos = at;
            editable_of(w).insert_text(text, &mut pos);
        }
    }
}

fn delete(src: &Src, start: i32, end: i32) {
    match src {
        Src::Buffer { buf, .. } => {
            let (mut s, mut e) = (buf.iter_at_offset(start), buf.iter_at_offset(end));
            buf.delete(&mut s, &mut e);
        }
        Src::Editable { w } => editable_of(w).delete_text(start, end),
    }
}

fn replace(src: &Src, start: i32, end: i32, text: &str) {
    match src {
        Src::Buffer { buf, .. } => {
            buf.begin_user_action();
            delete(src, start, end);
            insert(src, start, text);
            buf.end_user_action();
        }
        Src::Editable { .. } => {
            delete(src, start, end);
            insert(src, start, text);
        }
    }
}
