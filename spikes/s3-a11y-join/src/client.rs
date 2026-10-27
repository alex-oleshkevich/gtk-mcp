//! AT-SPI side: zbus client on the private a11y bus, parallel structural join
//! against the tree the app dumped through public `GtkAccessible` getters.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const ACC: &str = "org.a11y.atspi.Accessible";
const COMP: &str = "org.a11y.atspi.Component";

struct Bus {
    conn: Connection,
    calls: std::cell::Cell<u64>,
}

impl Bus {
    fn call<T>(&self, dest: &str, path: &str, iface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> zbus::Result<T>
    where
        T: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        self.calls.set(self.calls.get() + 1);
        let m = self.conn.call_method(Some(dest), path, Some(iface), method, body)?;
        m.body().deserialize::<T>()
    }
    fn prop(&self, dest: &str, path: &str, iface: &str, name: &str) -> Option<OwnedValue> {
        self.calls.set(self.calls.get() + 1);
        let m = self.conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "Get", &(iface, name)).ok()?;
        let body = m.body();
        let v: zbus::zvariant::Value = body.deserialize().ok()?;
        v.try_to_owned().ok()
    }
    fn prop_str(&self, dest: &str, path: &str, iface: &str, name: &str) -> Option<String> {
        self.prop(dest, path, iface, name).and_then(|v| String::try_from(v).ok())
    }
}

fn session_bus_a11y_address() -> String {
    if let Ok(a) = std::env::var("AT_SPI_BUS_ADDRESS") {
        return a;
    }
    let sess = zbus::blocking::Connection::session().expect("session bus");
    let m = sess
        .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
        .expect("org.a11y.Bus.GetAddress");
    m.body().deserialize::<String>().unwrap()
}

#[derive(Debug, Clone)]
struct ANode {
    dest: String,
    path: String,
    role: u32,
    role_name: String,
    name: String,
    description: String,
    states: Vec<u32>,
    interfaces: Vec<String>,
    id: Option<String>,
    win: Option<(i32, i32, i32, i32)>,
    parent: Option<(i32, i32, i32, i32)>,
    attrs: BTreeMap<String, String>,
    index_in_parent: i32,
    child_count_prop: i32,
    children: Vec<ANode>,
}

fn read(bus: &Bus, dest: &str, path: &str, depth: usize, nodes: &mut usize, deadline: Instant) -> ANode {
    *nodes += 1;
    let g = |m: &str| bus.call::<u32>(dest, path, ACC, m, &()).unwrap_or(u32::MAX);
    let interfaces = bus.call::<Vec<String>>(dest, path, ACC, "GetInterfaces", &()).unwrap_or_default();
    let has = |i: &str| interfaces.iter().any(|x| x == i);
    let ext = |ct: u32| -> Option<(i32, i32, i32, i32)> {
        if !has(COMP) {
            return None;
        }
        bus.call::<(i32, i32, i32, i32)>(dest, path, COMP, "GetExtents", &(ct,)).ok()
    };
    let mut n = ANode {
        dest: dest.into(),
        path: path.into(),
        role: g("GetRole"),
        role_name: bus.call::<String>(dest, path, ACC, "GetRoleName", &()).unwrap_or_default(),
        name: bus.prop_str(dest, path, ACC, "Name").unwrap_or_default(),
        description: bus.prop_str(dest, path, ACC, "Description").unwrap_or_default(),
        states: bus.call::<Vec<u32>>(dest, path, ACC, "GetState", &()).unwrap_or_default(),
        id: bus.prop_str(dest, path, ACC, "AccessibleId").filter(|s| !s.is_empty()),
        win: ext(1),
        parent: ext(2),
        attrs: bus.call::<BTreeMap<String, String>>(dest, path, ACC, "GetAttributes", &()).unwrap_or_default(),
        index_in_parent: bus.call::<i32>(dest, path, ACC, "GetIndexInParent", &()).unwrap_or(-2),
        child_count_prop: bus.prop(dest, path, ACC, "ChildCount").and_then(|v| i32::try_from(v).ok()).unwrap_or(-1),
        interfaces,
        children: vec![],
    };
    if depth < 64 && Instant::now() < deadline {
        let kids: Vec<(String, OwnedObjectPath)> = bus.call(dest, path, ACC, "GetChildren", &()).unwrap_or_default();
        for (d, p) in kids {
            n.children.push(read(bus, &d, p.as_str(), depth + 1, nodes, deadline));
        }
    }
    n
}

fn gtk_name(g: &Value) -> String {
    format!("{}({})", g["role"].as_str().unwrap_or("?"), g["widget_name"].as_str().unwrap_or(""))
}

#[derive(Default)]
struct Stats {
    gtk_nodes: usize,
    atspi_nodes: usize,
    joined: usize,
    count_mismatch_nodes: usize,
    unjoined_gtk: usize,
    unjoined_atspi: usize,
    bounds_equal_parentrel: usize,
    bounds_checked: usize,
    role_pairs: BTreeMap<String, BTreeMap<String, usize>>,
    mismatches: Vec<Value>,
    joined_rows: Vec<Value>,
}

fn count(g: &Value) -> usize {
    1 + g["children"].as_array().map(|c| c.iter().map(count).sum()).unwrap_or(0)
}
fn count_a(a: &ANode) -> usize {
    1 + a.children.iter().map(count_a).sum::<usize>()
}

fn bounds_of(g: &Value) -> Option<(i32, i32, i32, i32)> {
    let b = g["bounds"].as_array()?;
    Some((b[0].as_i64()? as i32, b[1].as_i64()? as i32, b[2].as_i64()? as i32, b[3].as_i64()? as i32))
}

/// Parallel positional walk with verification by child count; where counts
/// differ the subtree is reported and aligned by greedy bounds+order matching.
fn join(g: &Value, a: &ANode, path: &str, st: &mut Stats) {
    st.joined += 1;
    st.role_pairs.entry(g["role"].as_str().unwrap_or("?").into()).or_default().entry(a.role_name.clone()).and_modify(|c| *c += 1).or_insert(1);
    if let (Some(gb), Some(pb)) = (bounds_of(g), a.parent) {
        st.bounds_checked += 1;
        if gb == pb {
            st.bounds_equal_parentrel += 1;
        }
    }
    st.joined_rows.push(json!({
        "path": path, "gtk": gtk_name(g), "gtk_type": g["gtk_type"], "atspi_role": a.role_name,
        "atspi_name": a.name, "atspi_id": a.id, "gtk_id": g["accessible_id"], "gtk_bounds": g["bounds"],
        "atspi_parent": a.parent, "atspi_win": a.win, "atspi_path": a.path,
    }));
    let gc = g["children"].as_array().cloned().unwrap_or_default();
    if gc.len() == a.children.len() {
        for (i, (gg, aa)) in gc.iter().zip(&a.children).enumerate() {
            join(gg, aa, &format!("{path}/{i}"), st);
        }
        return;
    }
    st.count_mismatch_nodes += 1;
    st.mismatches.push(json!({
        "path": path, "gtk": gtk_name(g), "gtk_type": g["gtk_type"], "gtk_children": gc.len(), "atspi_children": a.children.len(),
        "gtk_child_roles": gc.iter().map(gtk_name).collect::<Vec<_>>(),
        "atspi_child_roles": a.children.iter().map(|c| format!("{}({})", c.role_name, c.name)).collect::<Vec<_>>(),
    }));
    // Greedy in-order alignment by parent-relative bounds.
    let mut ai = 0;
    for (gi, gg) in gc.iter().enumerate() {
        let mut found = None;
        for k in ai..a.children.len() {
            if bounds_of(gg).is_some() && bounds_of(gg) == a.children[k].parent {
                found = Some(k);
                break;
            }
        }
        match found {
            Some(k) => {
                st.unjoined_atspi += count_a_slice(&a.children[ai..k]);
                join(gg, &a.children[k], &format!("{path}/{gi}~"), st);
                ai = k + 1;
            }
            None => st.unjoined_gtk += count(gg),
        }
    }
    st.unjoined_atspi += count_a_slice(&a.children[ai.min(a.children.len())..]);
}

fn count_a_slice(s: &[ANode]) -> usize {
    s.iter().map(count_a).sum()
}

fn a_json(a: &ANode, depth: usize) -> Value {
    json!({"role": a.role_name, "name": a.name, "id": a.id, "win": a.win, "parent": a.parent, "ifaces": a.interfaces.len(),
        "idx": a.index_in_parent, "childcount": a.child_count_prop, "attrs": a.attrs,
        "children": if depth < 64 { a.children.iter().map(|c| a_json(c, depth + 1)).collect::<Vec<_>>() } else { vec![] }})
}

pub fn run(pid: u32, gtk_json: &str, out: &str) {
    let t0 = Instant::now();
    let addr = session_bus_a11y_address();
    eprintln!("a11y bus address: {addr}");
    let conn = Builder::address(addr.as_str()).unwrap().build().expect("connect a11y bus");
    let bus = Bus { conn, calls: Default::default() };
    // Registry root -> application roots.
    let apps: Vec<(String, OwnedObjectPath)> =
        bus.call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", ACC, "GetChildren", &()).expect("registry GetChildren");
    let mut found = None;
    let mut seen = vec![];
    for (d, p) in &apps {
        let upid = bus
            .call::<u32>("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", &(d.as_str(),))
            .ok();
        seen.push(json!({"bus_name": d, "path": p.as_str(), "pid": upid}));
        if upid == Some(pid) {
            found = Some((d.clone(), p.as_str().to_string()));
        }
    }
    let (dest, path) = found.unwrap_or_else(|| panic!("app pid {pid} not on the a11y bus; registry children: {seen:?}"));
    let find_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    let mut nodes = 0;
    let root = read(&bus, &dest, &path, 0, &mut nodes, Instant::now() + Duration::from_secs(60));
    let read_ms = t1.elapsed().as_millis();
    let app_attrs = bus.call::<BTreeMap<String, String>>(&dest, &path, ACC, "GetAttributes", &()).unwrap_or_default();
    let app_iface_id = bus.prop_str(&dest, &path, "org.a11y.atspi.Application", "Id");
    let toolkit = bus.prop_str(&dest, &path, "org.a11y.atspi.Application", "ToolkitName");
    let toolkit_ver = bus.prop_str(&dest, &path, "org.a11y.atspi.Application", "Version");

    let gtk: Value = serde_json::from_slice(&std::fs::read(gtk_json).unwrap()).unwrap();
    let mut report = serde_json::Map::new();
    report.insert("find_app_ms".into(), json!(find_ms));
    report.insert("read_ms".into(), json!(read_ms));
    report.insert("atspi_nodes".into(), json!(nodes));
    report.insert("dbus_calls".into(), json!(bus.calls.get()));
    report.insert("registry_children".into(), json!(seen));
    report.insert("app_root".into(), json!({"name": root.name, "role": root.role_name, "attrs": app_attrs, "app_id": app_iface_id, "toolkit": toolkit, "toolkit_version": toolkit_ver, "children": root.children.len()}));
    for key in ["closed", "popover_open"] {
        let g = &gtk[key];
        let mut st = Stats::default();
        let tops = g["toplevels"].as_array().unwrap();
        st.gtk_nodes = tops.iter().map(count).sum();
        st.atspi_nodes = count_a(&root) - 1;
        // Toplevel alignment: by title == AT-SPI name, falling back to order.
        let mut used = vec![false; root.children.len()];
        let mut top_report = vec![];
        for (ti, t) in tops.iter().enumerate() {
            let title = t["title"].as_str().unwrap_or("");
            let k = root.children.iter().enumerate().position(|(i, c)| !used[i] && !title.is_empty() && c.name == title);
            top_report.push(json!({"gtk_index": ti, "gtk_title": title, "atspi_index": k, "gtk_type": t["gtk_type"], "gtk_children": t["children"].as_array().map(|c| c.len())}));
            match k {
                Some(k) => {
                    used[k] = true;
                    join(t, &root.children[k], &format!("w{ti}"), &mut st);
                }
                None => st.unjoined_gtk += count(t),
            }
        }
        for (i, c) in root.children.iter().enumerate() {
            if !used[i] {
                st.unjoined_atspi += count_a(c);
            }
        }
        report.insert(
            key.into(),
            json!({
                "toplevel_alignment": top_report,
                "gtk_nodes": st.gtk_nodes, "atspi_nodes_below_app": st.atspi_nodes,
                "joined": st.joined, "count_mismatch_nodes": st.count_mismatch_nodes,
                "unjoined_gtk": st.unjoined_gtk, "unjoined_atspi": st.unjoined_atspi,
                "bounds_checked": st.bounds_checked, "bounds_equal_parent_relative": st.bounds_equal_parentrel,
                "role_pairs": st.role_pairs, "mismatches": st.mismatches, "rows": st.joined_rows,
            }),
        );
    }
    report.insert("atspi_tree".into(), a_json(&root, 0));
    std::fs::write(out, serde_json::to_vec_pretty(&Value::Object(report)).unwrap()).unwrap();
    eprintln!("s3 client: report written to {out} ({nodes} AT-SPI nodes, {read_ms} ms)");
}
