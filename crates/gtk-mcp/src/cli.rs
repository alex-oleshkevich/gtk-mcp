//! Shell-facing helpers behind `gtk-mcp apps|state|call|tools|prime`. They go through the
//! same [`Broker`] code path as the MCP server.

use crate::session::{Progress, ProgressSink, Session};
use crate::{Broker, ToolOutcome};
use gtk_mcp_protocol::{AgentError, ErrorCode, SessionState, TOOLS, TOOLSETS, tool_spec};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `tools` output: toolsets in registry order, each with its tools marked R or M.
pub fn tools_json() -> Value {
    Value::Array(
        TOOLSETS
            .iter()
            .map(|ts| {
                let tools: Vec<Value> = TOOLS
                    .iter()
                    .filter(|t| t.toolset == *ts)
                    .map(|t| json!({"name": t.name, "mutating": t.mutating}))
                    .collect();
                json!({"toolset": ts, "tools": tools})
            })
            .collect(),
    )
}

pub fn tools_text() -> String {
    let mut o = String::new();
    for ts in TOOLSETS {
        let tools: Vec<_> = TOOLS.iter().filter(|t| t.toolset == *ts).collect();
        o.push_str(&format!("{ts} ({})\n", tools.len()));
        for t in tools {
            o.push_str(&format!("  {} {}\n", if t.mutating { "M" } else { "R" }, t.name));
        }
    }
    let mutating = TOOLS.iter().filter(|t| t.mutating).count();
    o.push_str(&format!("\n{} tools, {mutating} mutating (M), {} read-only (R)\n", TOOLS.len(), TOOLS.len() - mutating));
    o
}

fn unknown_tool(tool: &str) -> AgentError {
    let close = close_matches(tool);
    let hint = if close.is_empty() {
        "run `gtk-mcp tools` to list them".to_owned()
    } else {
        format!("did you mean: {}?", close.join(", "))
    };
    AgentError::invalid_params(format!("unknown tool '{tool}'; {hint}"))
}

/// `tools NAME`: the tool's MCP definition (what an MCP client sees in `tools/list`)
/// plus its toolset and R/M flag.
pub fn tool_json(defs: &[rmcp::model::Tool], name: &str) -> Result<Value, AgentError> {
    let spec = tool_spec(name).ok_or_else(|| unknown_tool(name))?;
    let def = defs
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| AgentError::new(ErrorCode::NotFound, format!("tool '{name}' has no MCP definition")))?;
    Ok(json!({
        "name": spec.name,
        "toolset": spec.toolset,
        "mutating": spec.mutating,
        "description": def.description,
        "input_schema": def.input_schema,
    }))
}

pub fn tool_text(defs: &[rmcp::model::Tool], name: &str) -> Result<String, AgentError> {
    let v = tool_json(defs, name)?;
    let flag = if v["mutating"] == true { "M" } else { "R" };
    Ok(format!(
        "{} ({}, {flag})\n{}\n\nARGS_JSON schema:\n{}\n",
        name,
        v["toolset"].as_str().unwrap_or_default(),
        v["description"].as_str().unwrap_or_default(),
        serde_json::to_string_pretty(&v["input_schema"]).unwrap_or_default(),
    ))
}

/// `prime`: the agent guide followed by the tool index.
pub fn prime_text() -> String {
    format!("{}\n## Tool index\n\n```text\n{}```\n", include_str!("prime.md").trim_end(), tools_text())
}

fn state_label(s: &SessionState) -> String {
    match s {
        SessionState::Running => "running".into(),
        SessionState::Incompatible => "incompatible".into(),
        SessionState::Exited { code: Some(c), .. } => format!("exited ({c})"),
        SessionState::Exited { signal: Some(s), .. } => format!("exited (signal {s})"),
        SessionState::Exited { .. } => "exited".into(),
    }
}

/// `apps` as a table: session, pid, app id, GTK version, state.
pub fn apps_table(sessions: &[Arc<Session>]) -> String {
    if sessions.is_empty() {
        return "no instrumented apps are running\n".into();
    }
    let mut rows = vec![["SESSION", "PID", "APP ID", "GTK", "STATE"].map(String::from)];
    for s in sessions {
        let i = s.info();
        let gtk = i.hello.as_ref().map_or("-".into(), |h| format!("{}.{}.{}", h.gtk_version[0], h.gtk_version[1], h.gtk_version[2]));
        rows.push([i.session, i.pid.to_string(), i.app_id.unwrap_or_else(|| "-".into()), gtk, state_label(&i.state)]);
    }
    let widths: Vec<usize> = (0..5).map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0)).collect();
    let mut o = String::new();
    for r in &rows {
        let line: Vec<String> = r.iter().zip(&widths).map(|(cell, w)| format!("{cell:<w$}")).collect();
        o.push_str(line.join("  ").trim_end());
        o.push('\n');
    }
    o
}

fn describe(sessions: &[Arc<Session>]) -> String {
    if sessions.is_empty() {
        return "none".into();
    }
    sessions
        .iter()
        .map(|s| {
            let i = s.info();
            format!("{} (pid {}, app_id {}, {})", i.session, i.pid, i.app_id.as_deref().unwrap_or("-"), state_label(&i.state))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `--app`: a session id, a pid or an app id.
pub fn resolve_app(sessions: &[Arc<Session>], app: &str) -> Result<Arc<Session>, AgentError> {
    let pid = app.parse::<u32>().ok();
    let hits: Vec<Arc<Session>> = sessions
        .iter()
        .filter(|s| {
            let i = s.info();
            i.session == app || Some(i.pid) == pid || i.app_id.as_deref() == Some(app)
        })
        .cloned()
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(AgentError::invalid_params(format!("no app matches '{app}'; apps: {}", describe(sessions)))),
        _ => Err(AgentError::invalid_params(format!("'{app}' matches several apps; use a session id or pid: {}", describe(&hits)))),
    }
}

/// List registered app-state resources or read one, using the same resource APIs as MCP.
pub async fn state(broker: &Arc<Broker>, name: Option<&str>, app: Option<&str>, json: bool) -> Result<String, AgentError> {
    let mut resources = broker.list_state_resources().await;
    let sessions = broker.sessions();
    if let Some(app) = app {
        let session = resolve_app(&sessions, app)?;
        resources.retain(|r| r.session == session.id());
    }
    if let Some(name) = name {
        let matches: Vec<_> = resources.iter().filter(|r| r.name == name).collect();
        let resource = match matches.as_slice() {
            [resource] => resource,
            [] => {
                let mut names: Vec<_> = resources.iter().map(|r| r.name.as_str()).collect();
                names.sort_unstable();
                names.dedup();
                return Err(AgentError::new(
                    ErrorCode::NotFound,
                    format!("unknown state '{name}'; available names: {}", if names.is_empty() { "none".into() } else { names.join(", ") }),
                ));
            }
            _ => {
                let candidates: Vec<_> = sessions.iter().filter(|s| matches.iter().any(|r| r.session == s.id())).cloned().collect();
                return Err(AgentError::invalid_params(format!(
                    "state '{name}' is registered by several apps; use --app with a session id, pid or app id: {}",
                    describe(&candidates)
                )));
            }
        };
        let value = broker.read_state_resource(&resource.uri).await?;
        return Ok(format!("{}\n", serde_json::to_string_pretty(&value).unwrap_or_default()));
    }

    #[derive(serde::Serialize)]
    struct Listing {
        session: String,
        app: Option<String>,
        name: String,
    }
    let rows: Vec<_> = resources
        .into_iter()
        .map(|r| Listing {
            app: sessions.iter().find(|s| s.id() == r.session).and_then(|s| s.info().app_id),
            session: r.session,
            name: r.name,
        })
        .collect();
    if json {
        return Ok(format!("{}\n", serde_json::to_string_pretty(&rows).unwrap_or_default()));
    }
    if rows.is_empty() {
        return Ok("no registered app states\n".into());
    }
    let session_width = rows.iter().map(|r| r.session.chars().count()).max().unwrap_or(0).max("SESSION".len());
    let app_width = rows.iter().map(|r| r.app.as_deref().unwrap_or("-").chars().count()).max().unwrap_or(0).max("APP ID".len());
    let mut text = format!("{:<session_width$}  {:<app_width$}  NAME\n", "SESSION", "APP ID");
    for row in rows {
        text.push_str(&format!("{:<session_width$}  {:<app_width$}  {}\n", row.session, row.app.as_deref().unwrap_or("-"), row.name));
    }
    Ok(text)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Registered tools close to `name`: substring matches, then small edit distance.
pub fn close_matches(name: &str) -> Vec<&'static str> {
    let mut scored: Vec<(usize, &'static str)> = TOOLS
        .iter()
        .filter_map(|t| {
            let d = distance(name, t.name);
            let sub = !name.is_empty() && (t.name.contains(name) || name.contains(t.name));
            if sub {
                Some((0, t.name))
            } else if d <= 3.max(name.len() / 3) {
                Some((d, t.name))
            } else {
                None
            }
        })
        .collect();
    scored.sort();
    scored.into_iter().take(5).map(|(_, n)| n).collect()
}

/// `foo.png` plus `n` (2, 3, ...) becomes `foo-2.png`.
fn numbered(path: &Path, n: usize) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match path.extension() {
        Some(e) => format!("{stem}-{n}.{}", e.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

/// What `call` prints.
#[derive(Debug)]
pub struct CallOutput {
    /// The result for stdout (the JSON pretty-printed; non-JSON text as is).
    pub stdout: String,
    /// Notes for stderr.
    pub notes: Vec<String>,
}

/// Write the outcome's PNGs: the first to `out`, further ones to `out-2`, `out-3`, ...
pub fn save_images(outcome: &ToolOutcome, out: &Path) -> Result<Vec<PathBuf>, AgentError> {
    let mut written = Vec::new();
    for (i, png) in outcome.images().into_iter().enumerate() {
        let path = if i == 0 { out.to_owned() } else { numbered(out, i + 1) };
        std::fs::write(&path, png)
            .map_err(|e| AgentError::new(ErrorCode::Unavailable, format!("cannot write {}: {e}", path.display())))?;
        written.push(path);
    }
    Ok(written)
}

/// Run `tool` through the broker. `app` fills `session` unless `args` has one.
/// `args` is `None` for the default `{}`.
pub async fn call(
    broker: &Arc<Broker>,
    tool: &str,
    args: Option<Value>,
    app: Option<&str>,
    out: Option<&Path>,
) -> Result<CallOutput, AgentError> {
    if tool_spec(tool).is_none() {
        return Err(unknown_tool(tool));
    }
    let mut args = match args.unwrap_or_else(|| json!({})) {
        Value::Null => json!({}),
        v @ Value::Object(_) => v,
        _ => return Err(AgentError::invalid_params("ARGS_JSON must be a JSON object")),
    };
    if let Some(app) = app {
        let has_session = args.get("session").is_some_and(|s| !s.is_null() && s.as_str() != Some(""));
        if !has_session {
            broker.rescan().await;
            let s = resolve_app(&broker.sessions(), app)?;
            args["session"] = Value::String(s.id().to_owned());
        }
    }
    let progress: ProgressSink = Arc::new(|p: Progress| {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "gtk-mcp: progress {}", p.progress);
        if let Some(total) = p.total {
            let _ = write!(stderr, "/{total}");
        }
        if let Some(message) = p.message {
            for part in message.split(['\r', '\n']) {
                let _ = write!(stderr, " {part}");
            }
        }
        let _ = writeln!(stderr);
    });
    let outcome = broker.call_tool_with_progress(tool, args, Some(progress)).await?;
    let mut notes = Vec::new();
    if matches!(tool, "launch_app" | "restart_app") {
        notes.push("apps launched here stop when this command exits".to_owned());
    }
    match out {
        Some(path) => {
            let written = save_images(&outcome, path)?;
            if written.is_empty() {
                notes.push("the tool returned no image; nothing written".to_owned());
            }
            for p in written {
                notes.push(format!("wrote {}", p.display()));
            }
        }
        None => {
            let n = outcome.images().len();
            if n > 0 {
                notes.push(format!("{n} image(s) not shown; pass -o FILE to save them"));
            }
        }
    }
    let text = outcome.text_content();
    let stdout = match serde_json::from_str::<Value>(text) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.to_owned()),
        Err(_) => text.to_owned(),
    };
    Ok(CallOutput { stdout, notes })
}
