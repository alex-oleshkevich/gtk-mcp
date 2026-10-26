//! AT-SPI client (zbus) for the broker. Everything here talks to the app's
//! accessibility export over the a11y bus; the app itself is never asked to
//! call itself (plan 4: the broker owns AT-SPI, no self-call deadlock).
//!
//! The a11y bus address comes from `org.a11y.Bus.GetAddress` on the session
//! bus (`DBUS_SESSION_BUS_ADDRESS`, or [`BusConfig`]), exactly like libatspi.
//! The app is found by PID among the registry's application roots.

use futures_util::future::join_all;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const IFACE_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const CALL_TIMEOUT: Duration = Duration::from_secs(4);

/// Where to find the buses; tests inject a private bus here.
#[derive(Debug, Clone, Default)]
pub struct BusConfig {
    /// Session bus address; default `DBUS_SESSION_BUS_ADDRESS`.
    pub session_bus_address: Option<String>,
    /// Skip the session-bus lookup and connect to this a11y bus directly.
    pub a11y_bus_address: Option<String>,
}

/// Names of `AtspiStateType` bits in order.
const STATES: &[&str] = &[
    "invalid", "active", "armed", "busy", "checked", "collapsed", "defunct", "editable", "enabled", "expandable",
    "expanded", "focusable", "focused", "has-tooltip", "horizontal", "iconified", "modal", "multi-line",
    "multiselectable", "opaque", "pressed", "resizable", "selectable", "selected", "sensitive", "showing",
    "single-line", "stale", "transient", "vertical", "visible", "manages-descendants", "indeterminate", "required",
    "truncated", "animated", "invalid-entry", "supports-autocompletion", "selectable-text", "is-default", "visited",
    "checkable", "has-popup", "read-only",
];

pub fn state_names(bits: &[u32]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for (i, name) in STATES.iter().enumerate() {
        let word = bits.get(i / 32).copied().unwrap_or(0);
        if word & (1 << (i % 32)) != 0 {
            out.push(*name);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct AtNode {
    pub bus: String,
    pub path: String,
    pub role: u32,
    pub role_name: String,
    pub name: String,
    pub description: String,
    pub states: Vec<&'static str>,
    pub interfaces: Vec<String>,
    pub accessible_id: Option<String>,
    pub children: Vec<AtNode>,
    pub error: Option<String>,
}

impl AtNode {
    pub fn has_state(&self, s: &str) -> bool {
        self.states.contains(&s)
    }
    pub fn has_interface(&self, i: &str) -> bool {
        self.interfaces.iter().any(|x| x == i)
    }
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(AtNode::count).sum::<usize>()
    }
}

/// An application root in the registry.
#[derive(Debug, Clone)]
pub struct AppRoot {
    pub bus: String,
    pub path: String,
    pub pid: u32,
}

#[derive(Clone)]
pub struct Atspi {
    pub(crate) conn: Connection,
    address: String,
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn err(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

async fn call<B, R>(conn: &Connection, dest: &str, path: &str, iface: &str, member: &str, body: &B) -> Result<R, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let fut = conn.call_method(Some(dest.to_owned()), path.to_owned(), Some(iface.to_owned()), member.to_owned(), body);
    let msg = tokio::time::timeout(CALL_TIMEOUT, fut)
        .await
        .map_err(|_| format!("{iface}.{member} on {dest}{path} timed out after {CALL_TIMEOUT:?}"))?
        .map_err(|e| err(&format!("{iface}.{member} on {dest}{path}"), e))?;
    msg.body().deserialize().map_err(|e| err(&format!("decode {iface}.{member}"), e))
}

impl Atspi {
    /// Connect to the a11y bus. `Err` is a human-readable reason (surfaced as `unavailable`).
    pub async fn connect(cfg: &BusConfig) -> Result<Self, String> {
        let address = match cfg.a11y_bus_address.clone().or_else(|| std::env::var("AT_SPI_BUS_ADDRESS").ok().filter(|s| !s.is_empty()).filter(|_| cfg.session_bus_address.is_none())) {
            Some(a) => a,
            None => {
                let session = match &cfg.session_bus_address {
                    Some(a) => zbus::connection::Builder::address(a.as_str())
                        .map_err(|e| err("session bus address", e))?
                        .build()
                        .await
                        .map_err(|e| err("connect to the session bus", e))?,
                    None => Connection::session().await.map_err(|e| err("connect to the session bus", e))?,
                };
                let reply = tokio::time::timeout(
                    Duration::from_secs(10),
                    session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &()),
                )
                .await
                .map_err(|_| "org.a11y.Bus.GetAddress timed out (is at-spi-bus-launcher installed?)".to_string())?
                .map_err(|e| err("org.a11y.Bus.GetAddress (is at-spi-bus-launcher installed?)", e))?;
                reply.body().deserialize::<String>().map_err(|e| err("decode GetAddress", e))?
            }
        };
        let conn = zbus::connection::Builder::address(address.as_str())
            .map_err(|e| err("a11y bus address", e))?
            .build()
            .await
            .map_err(|e| err("connect to the a11y bus", e))?;
        Ok(Self { conn, address })
    }

    /// A second connection for signal streams. A stream's bounded queue applies
    /// backpressure to its whole connection, so method calls must never share
    /// one with a stream (a slow consumer would stall every reply).
    pub async fn connect_events(&self) -> Result<Connection, String> {
        zbus::connection::Builder::address(self.address.as_str())
            .map_err(|e| err("a11y bus address", e))?
            .max_queued(4096)
            .build()
            .await
            .map_err(|e| err("connect to the a11y bus", e))
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// The application root whose D-Bus connection belongs to `pid`.
    pub async fn find_app(&self, pid: u32) -> Result<AppRoot, String> {
        let kids: Vec<(String, OwnedObjectPath)> =
            call(&self.conn, REGISTRY, ROOT_PATH, IFACE_ACCESSIBLE, "GetChildren", &()).await?;
        let mut seen = Vec::new();
        for (bus, path) in kids {
            let p: u32 = match call(&self.conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", &(bus.as_str(),)).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            if p == pid {
                return Ok(AppRoot { bus, path: path.to_string(), pid });
            }
            seen.push(p);
        }
        Err(format!(
            "process {pid} is not registered on the a11y bus ({}; registered pids: {:?}); the app must run with GTK_A11Y=atspi on the same session bus",
            self.address, seen
        ))
    }

    async fn node(&self, bus: &str, path: &str) -> Result<(AtNode, Vec<(String, String)>), String> {
        let c = &self.conn;
        let props = async {
            let all: HashMap<String, OwnedValue> =
                call(c, bus, path, "org.freedesktop.DBus.Properties", "GetAll", &(IFACE_ACCESSIBLE,)).await?;
            Ok::<_, String>(all)
        };
        let (props, role, role_name, state, ifaces, kids) = tokio::join!(
            props,
            call::<_, u32>(c, bus, path, IFACE_ACCESSIBLE, "GetRole", &()),
            call::<_, String>(c, bus, path, IFACE_ACCESSIBLE, "GetRoleName", &()),
            call::<_, Vec<u32>>(c, bus, path, IFACE_ACCESSIBLE, "GetState", &()),
            call::<_, Vec<String>>(c, bus, path, IFACE_ACCESSIBLE, "GetInterfaces", &()),
            call::<_, Vec<(String, OwnedObjectPath)>>(c, bus, path, IFACE_ACCESSIBLE, "GetChildren", &()),
        );
        let text = |p: &HashMap<String, OwnedValue>, k: &str| -> String {
            p.get(k).and_then(|v| v.try_clone().ok()).and_then(|v| String::try_from(v).ok()).unwrap_or_default()
        };
        let (name, description, accessible_id) = match &props {
            Ok(p) => {
                let id = text(p, "AccessibleId");
                (text(p, "Name"), text(p, "Description"), (!id.is_empty()).then_some(id))
            }
            Err(_) => (String::new(), String::new(), None),
        };
        let mut errors: Vec<String> = Vec::new();
        fn take<T: Default>(r: Result<T, String>, errors: &mut Vec<String>) -> T {
            r.unwrap_or_else(|e| {
                errors.push(e);
                T::default()
            })
        }
        let role = take(role, &mut errors);
        let role_name = take(role_name, &mut errors);
        let states = state_names(&take(state, &mut errors));
        let interfaces = take(ifaces, &mut errors);
        let kids = take(kids, &mut errors);
        if let Err(e) = &props {
            errors.push(e.clone());
        }
        Ok((
            AtNode {
                bus: bus.to_owned(),
                path: path.to_owned(),
                role,
                role_name,
                name,
                description,
                states,
                interfaces,
                accessible_id,
                children: Vec::new(),
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            },
            kids.into_iter().map(|(b, p)| (b, p.to_string())).collect(),
        ))
    }

    /// Walk the app's accessible tree from its root. `max_nodes` bounds the walk.
    pub async fn tree(&self, root: &AppRoot, max_nodes: usize) -> Result<AtNode, String> {
        let budget = AtomicUsize::new(max_nodes);
        let r = tokio::time::timeout(Duration::from_secs(15), self.walk(root.bus.clone(), root.path.clone(), &budget, 0)).await;
        match r {
            Ok(n) => n,
            Err(_) => Err("AT-SPI tree walk timed out".into()),
        }
    }

    fn walk<'a>(&'a self, bus: String, path: String, budget: &'a AtomicUsize, depth: usize) -> BoxFut<'a, Result<AtNode, String>> {
        Box::pin(async move {
            if budget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1)).is_err() {
                return Err("AT-SPI node budget exhausted".into());
            }
            let (mut node, kids) = self.node(&bus, &path).await?;
            if depth < 64 {
                let results = join_all(kids.into_iter().map(|(b, p)| self.walk(b, p, budget, depth + 1))).await;
                for r in results {
                    match r {
                        Ok(n) => node.children.push(n),
                        Err(e) if e.contains("budget exhausted") => {
                            node.error.get_or_insert_with(|| e);
                        }
                        Err(e) => {
                            node.error = Some(e);
                        }
                    }
                }
            }
            Ok(node)
        })
    }

    /// Fields only inspect_accessible needs: attributes, relations, actions, extents, text, value.
    pub async fn details(&self, bus: &str, path: &str, interfaces: &[String], redact_content: bool) -> Value {
        let c = &self.conn;
        let has = |i: &str| interfaces.iter().any(|x| x.ends_with(i));
        let attrs = async {
            call::<_, HashMap<String, String>>(c, bus, path, IFACE_ACCESSIBLE, "GetAttributes", &()).await
        };
        let rels = async {
            call::<_, Vec<(u32, Vec<(String, OwnedObjectPath)>)>>(c, bus, path, IFACE_ACCESSIBLE, "GetRelationSet", &()).await
        };
        let extents = async {
            if !has("Component") {
                return Err("no Component interface".to_string());
            }
            // 0 = screen, 1 = window, 2 = parent coordinates.
            call::<_, (i32, i32, i32, i32)>(c, bus, path, "org.a11y.atspi.Component", "GetExtents", &(1u32,)).await
        };
        let actions = async {
            if !has("Action") {
                return Err("no Action interface".to_string());
            }
            let n: i32 = prop(c, bus, path, "org.a11y.atspi.Action", "NActions").await?;
            let mut out = Vec::new();
            for i in 0..n.clamp(0, 32) {
                let name: Result<String, _> = call(c, bus, path, "org.a11y.atspi.Action", "GetName", &(i,)).await;
                let desc: Result<String, _> = call(c, bus, path, "org.a11y.atspi.Action", "GetDescription", &(i,)).await;
                out.push(json!({"name": name.unwrap_or_default(), "description": desc.unwrap_or_default()}));
            }
            Ok(out)
        };
        let text = async {
            if !has("Text") || redact_content {
                return Err("no Text interface".to_string());
            }
            let n: i32 = prop(c, bus, path, "org.a11y.atspi.Text", "CharacterCount").await?;
            let s: String = call(c, bus, path, "org.a11y.atspi.Text", "GetText", &(0i32, n.clamp(0, 4096))).await?;
            Ok((n, s))
        };
        let value = async {
            if !has("Value") || redact_content {
                return Err("no Value interface".to_string());
            }
            let cur: f64 = prop(c, bus, path, "org.a11y.atspi.Value", "CurrentValue").await?;
            let min: f64 = prop(c, bus, path, "org.a11y.atspi.Value", "MinimumValue").await.unwrap_or(f64::NAN);
            let max: f64 = prop(c, bus, path, "org.a11y.atspi.Value", "MaximumValue").await.unwrap_or(f64::NAN);
            Ok(json!({"current": cur, "minimum": finite(min), "maximum": finite(max)}))
        };
        let (attrs, rels, extents, actions, text, value) = tokio::join!(attrs, rels, extents, actions, text, value);
        let mut m = serde_json::Map::new();
        if let Ok(a) = attrs {
            m.insert("attributes".into(), json!(a));
        }
        if let Ok(r) = rels {
            let rels: Vec<Value> = r
                .into_iter()
                .map(|(kind, targets)| {
                    json!({"relation": relation_name(kind), "targets": targets.into_iter().map(|(b, p)| json!({"bus": b, "path": p.to_string()})).collect::<Vec<_>>()})
                })
                .collect();
            m.insert("relations".into(), json!(rels));
        }
        if let Ok((x, y, w, h)) = extents {
            m.insert("extents".into(), json!({"x": x, "y": y, "width": w, "height": h, "coords": "window"}));
        }
        if let Ok(a) = actions {
            m.insert("actions".into(), json!(a));
        }
        if let Ok((n, s)) = text {
            m.insert("text".into(), json!({"character_count": n, "text": s}));
        }
        if let Ok(v) = value {
            m.insert("value".into(), v);
        }
        Value::Object(m)
    }
}

fn finite(f: f64) -> Value {
    if f.is_finite() { json!(f) } else { Value::Null }
}

async fn prop<T>(c: &Connection, bus: &str, path: &str, iface: &str, name: &str) -> Result<T, String>
where
    T: TryFrom<OwnedValue>,
    T::Error: std::fmt::Display,
{
    let v: OwnedValue = call(c, bus, path, "org.freedesktop.DBus.Properties", "Get", &(iface, name)).await?;
    T::try_from(v).map_err(|e| err(&format!("property {iface}.{name}"), e))
}

fn relation_name(k: u32) -> String {
    const NAMES: &[&str] = &[
        "null", "label-for", "labelled-by", "controller-for", "controlled-by", "member-of", "tooltip-for", "node-child-of",
        "node-parent-of", "extended", "flows-to", "flows-from", "subwindow-of", "embeds", "embedded-by", "popup-for",
        "parent-window-of", "description-for", "described-by", "details", "details-for", "error-message", "error-for",
    ];
    NAMES.get(k as usize).map_or_else(|| format!("relation-{k}"), |s| (*s).to_string())
}
