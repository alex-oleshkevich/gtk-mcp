//! The `a11y` toolset (M6): AT-SPI source, join with the agent's accessible
//! tree, and the tools built on it (`docs/tools.md` a11y).
//!
//! The agent answers the GTK side (`accessible_tree`, `inspect_accessible`);
//! this module adds AT-SPI when the broker runs with `--allow-atspi`.
//! Without it every AT-SPI field is reported unavailable and AT-SPI rules of
//! `audit_accessibility` as skipped (AC-13).

pub mod atspi;
pub mod audit;
pub mod events;
pub mod join;

use crate::broker::{Broker, ToolOutcome};
use crate::session::{ProgressSink, Session};
use atspi::{AtNode, Atspi};
use gtk_mcp_protocol::{AccessibleTreeParams, AgentError, AuditAccessibilityParams, ErrorCode, InspectAccessibleParams};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Node budget of the AT-SPI walk; the agent's own limit is below it.
const AT_BUDGET: usize = 6000;
const DEFAULT_MAX_NODES: u32 = 2000;

fn invalid(msg: impl Into<String>) -> AgentError {
    AgentError::invalid_params(msg)
}

/// What the AT-SPI side contributed to one request.
pub(crate) struct Source {
    /// `None`: unavailable, with `reason`.
    pub status: Result<join::JoinStats, String>,
    pub atspi: Option<Atspi>,
}

impl Source {
    pub fn available(&self) -> bool {
        self.status.is_ok()
    }
    pub fn reason(&self) -> &str {
        self.status.as_ref().err().map_or("", String::as_str)
    }
    pub fn status_json(&self) -> Value {
        match &self.status {
            Ok(stats) => json!({"available": true, "source": "atspi", "join": stats.to_json()}),
            Err(reason) => json!({"available": false, "source": "atspi", "reason": reason}),
        }
    }
}

impl Broker {
    /// The shared AT-SPI connection, created on first use (not on failure, so a
    /// bus that appears later is picked up).
    pub(crate) async fn atspi_client(&self) -> Result<Atspi, String> {
        if !self.policy.allow_atspi {
            return Err("AT-SPI is off: start the broker with --allow-atspi".into());
        }
        let mut g = self.atspi.lock().await;
        if let Some(a) = g.as_ref() {
            return Ok(a.clone());
        }
        let a = Atspi::connect(&self.config.atspi_bus).await?;
        *g = Some(a.clone());
        Ok(a)
    }

    /// AT-SPI tree of the session's process.
    async fn atspi_tree(&self, pid: u32) -> Result<(Atspi, AtNode), String> {
        let at = self.atspi_client().await?;
        let app = at.find_app(pid).await?;
        let tree = at.tree(&app, AT_BUDGET).await?;
        Ok((at, tree))
    }

    async fn agent_call(
        &self,
        session: &Session,
        method: &str,
        params: Value,
        progress: Option<ProgressSink>,
    ) -> Result<Value, AgentError> {
        session.request(method, params, self.config.default_deadline_ms, progress).await
    }

    /// Full accessible tree of all toplevels, joined with AT-SPI when available.
    /// Returns the agent response (with joined `roots`) and the source status.
    async fn joined_tree(
        &self,
        session: &Arc<Session>,
        max_nodes: Option<u32>,
        max_depth: Option<u32>,
        progress: Option<ProgressSink>,
    ) -> Result<(Value, Source), AgentError> {
        let mut params = json!({"session": session.id(), "max_nodes": max_nodes.unwrap_or(DEFAULT_MAX_NODES)});
        if let Some(d) = max_depth {
            params["max_depth"] = json!(d);
        }
        let mut resp = self.agent_call(session, "accessible_tree", params, progress).await?;
        let roots: Vec<Value> = resp["roots"].as_array().cloned().unwrap_or_default();
        let source = match self.atspi_tree(session.pid()).await {
            Ok((at, app)) => {
                let (joined, stats) = join::join_roots(&roots, &app);
                resp["roots"] = Value::Array(joined);
                Source { status: Ok(stats), atspi: Some(at) }
            }
            Err(reason) => {
                resp["roots"] = Value::Array(roots.into_iter().map(|r| mark_gtk_only(r)).collect());
                Source { status: Err(reason), atspi: None }
            }
        };
        Ok((resp, source))
    }

    pub(crate) async fn a11y(
        self: &Arc<Self>,
        name: &str,
        session: &Arc<Session>,
        args: Value,
        progress: Option<ProgressSink>,
    ) -> Result<ToolOutcome, AgentError> {
        match name {
            "accessible_tree" => {
                let p: AccessibleTreeParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
                self.accessible_tree(session, p, progress).await
            }
            "inspect_accessible" => {
                let p: InspectAccessibleParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
                self.inspect_accessible(session, p, progress).await
            }
            "audit_accessibility" => {
                let p: AuditAccessibilityParams = serde_json::from_value(args).map_err(|e| invalid(e.to_string()))?;
                self.audit_accessibility(session, p, progress).await
            }
            other => Err(invalid(format!("'{other}' is not an a11y tool"))),
        }
    }

    /// Ref of a widget target (resolving selectors through the agent).
    async fn target_ref(&self, session: &Session, target: &Value) -> Result<String, AgentError> {
        if let Some(r) = target.as_str() {
            return Ok(r.to_owned());
        }
        let r = self
            .agent_call(session, "inspect_accessible", json!({"session": session.id(), "target": target}), None)
            .await?;
        r["ref"].as_str().map(str::to_owned).ok_or_else(|| AgentError::new(ErrorCode::Unavailable, "agent returned no ref"))
    }

    async fn accessible_tree(
        &self,
        session: &Arc<Session>,
        p: AccessibleTreeParams,
        progress: Option<ProgressSink>,
    ) -> Result<ToolOutcome, AgentError> {
        let root = match &p.root {
            Some(t) => Some(self.target_ref(session, &serde_json::to_value(t).unwrap()).await?),
            None => None,
        };
        let (mut resp, source) = self.joined_tree(session, p.max_nodes, None, progress).await?;
        if let Some(r) = root {
            let node = join::find_ref(resp["roots"].as_array().map_or(&[][..], |v| v.as_slice()), &r)
                .cloned()
                .ok_or_else(|| AgentError::new(ErrorCode::NotFound, format!("{r} is not in the accessible tree")))?;
            resp["roots"] = json!([node]);
        }
        if let Some(d) = p.max_depth {
            if let Some(roots) = resp["roots"].as_array_mut() {
                for r in roots {
                    prune(r, d as usize);
                }
            }
        }
        resp["session"] = json!(session.id());
        resp["atspi"] = source.status_json();
        Ok(ToolOutcome::json(&resp))
    }

    async fn inspect_accessible(
        &self,
        session: &Arc<Session>,
        p: InspectAccessibleParams,
        progress: Option<ProgressSink>,
    ) -> Result<ToolOutcome, AgentError> {
        let mut gtk = self
            .agent_call(session, "inspect_accessible", json!({"session": session.id(), "target": p.target}), progress.clone())
            .await?;
        let r = gtk["ref"].as_str().unwrap_or_default().to_owned();
        let secret = gtk["secret"].as_bool().unwrap_or(false);
        let (resp, source) = self.joined_tree(session, None, None, progress).await?;
        let roots = resp["roots"].as_array().map_or(&[][..], |v| v.as_slice());
        let node = join::find_ref(roots, &r);

        let mut fields = Map::new();
        let tag = |v: Value| json!({"value": v, "source": "gtk"});
        for key in ["role", "bounds", "platform_state", "parent", "children", "sensitive", "mapped", "label", "tooltip", "icon_name", "buildable_id", "automation_id"] {
            if let Some(v) = gtk.get_mut(key).map(Value::take).filter(|v| !v.is_null()) {
                fields.insert(key.into(), tag(v));
            }
        }
        let mut join_kind = "unavailable";
        match (&source.status, node) {
            (Err(reason), _) => {
                for key in ["name", "description", "states", "relations", "actions", "text", "value", "attributes", "extents"] {
                    fields.insert(key.into(), json!({"available": false, "source": "atspi", "reason": reason}));
                }
            }
            (Ok(_), Some(n)) if n["atspi"].is_object() => {
                join_kind = if n["atspi"]["accessible_id"].is_string() && n["atspi"]["accessible_id"] == n["buildable_id"] { "id" } else { "structure" };
                let at = &n["atspi"];
                let bus = at["bus"].as_str().unwrap_or_default();
                let path = at["path"].as_str().unwrap_or_default();
                let interfaces: Vec<String> = at["interfaces"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                let atag = |v: Value| json!({"value": v, "source": "atspi"});
                if secret {
                    for key in ["name", "description", "text", "value"] {
                        fields.insert(key.into(), json!({"redacted": true, "source": "atspi"}));
                    }
                } else {
                    fields.insert("name".into(), atag(at["name"].clone()));
                    fields.insert("description".into(), atag(at["description"].clone()));
                }
                fields.insert("states".into(), atag(at["states"].clone()));
                fields.insert("role_atspi".into(), atag(at["role"].clone()));
                fields.insert("interfaces".into(), atag(json!(interfaces)));
                if let Some(c) = source.atspi.as_ref() {
                    let d = c.details(bus, path, &full_interfaces(&interfaces), secret).await;
                    let by_path = path_refs(roots);
                    if let Some(o) = d.as_object() {
                        for (k, v) in o {
                            if k == "relations" && !secret {
                                fields.insert(k.clone(), atag(resolve_relations(v, &by_path)));
                            } else if k != "text" && k != "value" || !secret {
                                fields.insert(k.clone(), atag(v.clone()));
                            }
                        }
                    }
                }
            }
            (Ok(_), _) => {
                join_kind = "unjoined";
                for key in ["name", "description", "states", "relations", "actions", "text", "value", "attributes", "extents"] {
                    fields.insert(key.into(), json!({"available": false, "source": "atspi", "reason": "no AT-SPI node is joined to this widget"}));
                }
            }
        }
        Ok(ToolOutcome::json(&json!({
            "session": session.id(),
            "ref": r,
            "type": gtk["type"],
            "secret": secret,
            "join": join_kind,
            "atspi": source.status_json(),
            "fields": Value::Object(fields),
        })))
    }

    async fn audit_accessibility(
        &self,
        session: &Arc<Session>,
        p: AuditAccessibilityParams,
        progress: Option<ProgressSink>,
    ) -> Result<ToolOutcome, AgentError> {
        let root = match &p.root {
            Some(t) => Some(self.target_ref(session, &serde_json::to_value(t).unwrap()).await?),
            None => None,
        };
        let (resp, source) = self.joined_tree(session, p.max_nodes, None, progress).await?;
        let mut roots: Vec<Value> = resp["roots"].as_array().cloned().unwrap_or_default();
        if let Some(r) = root {
            let node = join::find_ref(&roots, &r)
                .cloned()
                .ok_or_else(|| AgentError::new(ErrorCode::NotFound, format!("{r} is not in the accessible tree")))?;
            roots = vec![node];
        }
        let report = audit::run(&roots, &source, p.rules.as_deref())?;
        let mut out = report;
        out["session"] = json!(session.id());
        out["atspi"] = source.status_json();
        out["truncated"] = resp["truncated"].clone();
        Ok(ToolOutcome::json(&out))
    }
}

fn mark_gtk_only(mut n: Value) -> Value {
    if let Some(o) = n.as_object_mut() {
        o.insert("source".into(), json!("gtk"));
        if let Some(Value::Array(kids)) = o.get_mut("children") {
            for k in kids.iter_mut() {
                *k = mark_gtk_only(k.take());
            }
        }
    }
    n
}

fn prune(n: &mut Value, depth: usize) {
    if let Some(Value::Array(kids)) = n.get_mut("children") {
        if depth == 0 {
            if !kids.is_empty() {
                n["children_truncated"] = json!(true);
            }
            n["children"] = json!([]);
        } else {
            for k in kids.iter_mut() {
                prune(k, depth - 1);
            }
        }
    }
}

/// Interface names as D-Bus names (the join keeps short names).
fn full_interfaces(short: &[String]) -> Vec<String> {
    short.iter().map(|s| format!("org.a11y.atspi.{s}")).collect()
}

/// `(bus, path)` of every joined AT-SPI node mapped to the widget ref.
fn path_refs(roots: &[Value]) -> HashMap<(String, String), String> {
    fn go(n: &Value, out: &mut HashMap<(String, String), String>) {
        if let (Some(r), Some(b), Some(p)) = (n["ref"].as_str(), n["atspi"]["bus"].as_str(), n["atspi"]["path"].as_str()) {
            out.insert((b.to_owned(), p.to_owned()), r.to_owned());
        }
        for c in n["children"].as_array().into_iter().flatten() {
            go(c, out);
        }
    }
    let mut out = HashMap::new();
    for r in roots {
        go(r, &mut out);
    }
    out
}

fn resolve_relations(v: &Value, by_path: &HashMap<(String, String), String>) -> Value {
    let mut v = v.clone();
    for rel in v.as_array_mut().into_iter().flatten() {
        for t in rel["targets"].as_array_mut().into_iter().flatten() {
            let key = (t["bus"].as_str().unwrap_or_default().to_owned(), t["path"].as_str().unwrap_or_default().to_owned());
            t["ref"] = by_path.get(&key).map_or(Value::Null, |r| json!(r));
        }
    }
    v
}
