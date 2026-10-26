//! AT-SPI event source for `watch` (tools.md diagnostics): focus, state, name
//! and window events of one app from the a11y bus, mapped to widget refs
//! through the join and appended to the session's event ring as
//! `{"kind":"atspi", ...}` records.
//!
//! Secret widgets: their records carry the event kind and state only, never a
//! name.

use super::atspi::AppRoot;
use crate::broker::{Broker, lock};
use crate::session::Session;
use futures_util::StreamExt;
use gtk_mcp_protocol::{AgentError, ErrorCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use zbus::zvariant::OwnedValue;

/// Event interfaces subscribed on the a11y bus.
const INTERFACES: &[&str] = &["Object", "Window", "Focus"];
/// Minimum interval between join refreshes triggered by unknown objects.
const REFRESH_GAP: Duration = Duration::from_millis(300);

#[derive(Clone)]
struct Known {
    r#ref: Option<String>,
    role: String,
    name: Option<String>,
    secret: bool,
}

fn kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn collect(n: &Value, out: &mut HashMap<String, Known>) {
    if let Some(p) = n["atspi"]["path"].as_str() {
        let secret = n["secret"].as_bool().unwrap_or(false);
        out.insert(
            p.to_owned(),
            Known {
                r#ref: n["ref"].as_str().map(str::to_owned),
                role: n["atspi"]["role"].as_str().unwrap_or_default().to_owned(),
                name: if secret { None } else { n["atspi"]["name"].as_str().map(str::to_owned) },
                secret,
            },
        );
    }
    for c in n["children"].as_array().into_iter().flatten() {
        collect(c, out);
    }
}

/// The `read_events` record of one AT-SPI event. Secret objects carry the
/// event kind and state only: no name, and no detail numbers for events whose
/// details are text offsets or lengths (`object:text-changed:insert` carries the
/// position and length of the inserted text). An object missing from the join
/// (`k` is `None`) may be secret: it is recorded as `unresolved`, the same way.
fn event_record(event: &str, d1: i32, d2: i32, path: &str, k: Option<&Known>) -> Value {
    let mut rec = json!({
        "kind": "atspi",
        "source": "atspi",
        "subscription": "atspi",
        "event": event,
        "path": path,
        "ref": k.and_then(|k| k.r#ref.clone()),
        "role": k.map(|k| k.role.clone()),
    });
    let withheld = k.is_none_or(|k| k.secret);
    let state_only = ["object:state-changed", "focus:", "window:"].iter().any(|p| event.starts_with(p));
    if !withheld || state_only {
        rec["detail1"] = json!(d1);
        rec["detail2"] = json!(d2);
    }
    match k {
        None => rec["unresolved"] = json!(true),
        Some(k) if k.secret => rec["secret"] = json!(true),
        Some(k) => rec["name"] = json!(k.name),
    }
    rec
}

impl Broker {
    /// Start forwarding the AT-SPI events of `session`'s app into its event
    /// ring. `kinds` filters by event-name prefix (`object:state-changed:focused`,
    /// `window:`); empty or `None`: focus, state, name and window events.
    /// Restarting replaces the previous subscription of that session.
    pub async fn watch_atspi(self: &Arc<Self>, session: &Arc<Session>, kinds: Option<Vec<String>>) -> Result<Value, AgentError> {
        let unavailable = |m: String| AgentError::new(ErrorCode::Unavailable, m);
        let at = self.atspi_client().await.map_err(unavailable)?;
        let app: AppRoot = at.find_app(session.pid()).await.map_err(unavailable)?;
        let kinds: Vec<String> = kinds.unwrap_or_default();
        let kinds = if kinds.is_empty() {
            vec!["focus:".into(), "object:state-changed:".into(), "object:property-change:accessible-name".into(), "window:".into()]
        } else {
            kinds
        };

        let conn = at.connect_events().await.map_err(unavailable)?;
        for iface in INTERFACES {
            let rule = format!("type='signal',sender='{}',interface='org.a11y.atspi.Event.{iface}'", app.bus);
            conn.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "AddMatch", &(rule,))
                .await
                .map_err(|e| unavailable(format!("AddMatch on the a11y bus: {e}")))?;
        }
        // GTK exports events regardless; registering is what at-spi2-core clients do, so
        // toolkits that only emit for registered listeners also work. Failure is harmless.
        for ev in ["object:", "window:", "focus:"] {
            let _ = at
                .conn
                .call_method(
                    Some("org.a11y.atspi.Registry"),
                    "/org/a11y/atspi/registry",
                    Some("org.a11y.atspi.Registry"),
                    "RegisterEvent",
                    &(ev, Vec::<String>::new(), ""),
                )
                .await;
        }

        let mut stream = zbus::MessageStream::from(&conn);
        let weak: Weak<Broker> = Arc::downgrade(self);
        let sess = session.clone();
        let bus = app.bus.clone();
        let mut known: HashMap<String, Known> = HashMap::new();
        if let Ok((_, tree)) = self.atspi_tree_joined(session).await {
            for r in &tree {
                collect(r, &mut known);
            }
        }
        let filter = kinds.clone();
        let handle = tokio::spawn(async move {
            let mut last_refresh = Instant::now();
            while let Some(Ok(msg)) = stream.next().await {
                let header = msg.header();
                if header.message_type() != zbus::message::Type::Signal || header.sender().map(|s| s.as_str()) != Some(bus.as_str()) {
                    continue;
                }
                let (Some(iface), Some(member), Some(path)) = (header.interface(), header.member(), header.path()) else { continue };
                let Some(domain) = iface.as_str().strip_prefix("org.a11y.atspi.Event.") else { continue };
                let domain = domain.to_lowercase();
                let Ok((detail, d1, d2, _data, _props)) =
                    msg.body().deserialize::<(String, i32, i32, OwnedValue, HashMap<String, OwnedValue>)>()
                else {
                    continue;
                };
                let mut event = format!("{domain}:{}", kebab(member.as_str()));
                if !detail.is_empty() {
                    event.push(':');
                    event.push_str(&detail);
                }
                if !filter.iter().any(|k| event.starts_with(k.as_str())) {
                    continue;
                }
                let path = path.as_str().to_owned();
                if !known.contains_key(&path) && last_refresh.elapsed() >= REFRESH_GAP {
                    last_refresh = Instant::now();
                    if let Some(b) = weak.upgrade() {
                        if let Ok((_, roots)) = b.atspi_tree_joined(&sess).await {
                            known.clear();
                            for r in &roots {
                                collect(r, &mut known);
                            }
                        }
                    }
                }
                sess.push_event(event_record(&event, d1, d2, &path, known.get(&path)));
            }
        });
        if let Some(old) = lock(&self.atspi_watches).insert(session.id().to_owned(), handle) {
            old.abort();
        }
        Ok(json!({"session": session.id(), "subscription": "atspi", "kind": "atspi", "source": "atspi", "events": kinds, "app_bus_name": app.bus}))
    }

    /// Stop the AT-SPI subscription of a session; `false` when there was none.
    pub fn unwatch_atspi(&self, session_id: &str) -> bool {
        match lock(&self.atspi_watches).remove(session_id) {
            Some(h) => {
                h.abort();
                true
            }
            None => false,
        }
    }

    /// Joined roots (for ref mapping); errors when AT-SPI is unavailable.
    async fn atspi_tree_joined(&self, session: &Arc<Session>) -> Result<(super::Source, Vec<Value>), AgentError> {
        let (resp, source) = self.joined_tree(session, None, None, None).await?;
        if let Err(r) = &source.status {
            return Err(AgentError::new(ErrorCode::Unavailable, r.clone()));
        }
        Ok((source, resp["roots"].as_array().cloned().unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_records_carry_no_name_and_no_text_offsets() {
        let secret = Known { r#ref: Some("w9".into()), role: "password text".into(), name: None, secret: true };
        let r = event_record("object:text-changed:insert", 3, 1, "/p", Some(&secret));
        assert_eq!(r["secret"], true);
        for k in ["name", "detail1", "detail2"] {
            assert!(r.get(k).is_none(), "{k}: {r}");
        }
        let r = event_record("object:text-caret-moved", 4, 0, "/p", Some(&secret));
        assert!(r.get("detail1").is_none(), "{r}");
        // State changes keep their state value.
        let r = event_record("object:state-changed:focused", 1, 0, "/p", Some(&secret));
        assert_eq!((&r["detail1"], &r["detail2"]), (&json!(1), &json!(0)));

        let plain = Known { r#ref: Some("w1".into()), role: "text".into(), name: Some("Name".into()), secret: false };
        let r = event_record("object:text-changed:insert", 3, 1, "/q", Some(&plain));
        assert_eq!((&r["detail1"], &r["detail2"], &r["name"]), (&json!(3), &json!(1), &json!("Name")));
        assert!(r.get("secret").is_none());
    }

    /// gm-36d: an object the join does not know (refresh rate-limited or
    /// failed, outside the joined tree) may be a password field: it gets no
    /// name and no text offsets, only state details.
    #[test]
    fn unresolved_objects_fail_closed() {
        let r = event_record("object:text-changed:insert", 7, 1, "/u", None);
        assert_eq!(r["unresolved"], true);
        assert!(r["ref"].is_null() && r["role"].is_null(), "{r}");
        for k in ["name", "detail1", "detail2", "secret"] {
            assert!(r.get(k).is_none(), "{k}: {r}");
        }
        assert!(event_record("object:text-caret-moved", 8, 0, "/u", None).get("detail1").is_none());
        let r = event_record("object:state-changed:focused", 1, 0, "/u", None);
        assert_eq!((&r["detail1"], &r["detail2"]), (&json!(1), &json!(0)));
    }

    #[test]
    fn collected_secrecy_covers_unjoined_descendants() {
        let tree = json!({"ref": "w1", "secret": true, "atspi": {"path": "/e", "role": "password text"}, "children": [
            {"ref": null, "secret": true, "atspi": {"path": "/e/t", "role": "text", "redacted": true}, "children": []}
        ]});
        let mut known = HashMap::new();
        collect(&tree, &mut known);
        assert!(known["/e/t"].secret && known["/e/t"].name.is_none());
    }
}
