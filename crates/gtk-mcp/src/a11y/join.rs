//! Structural join of the agent's `GtkAccessible` tree with the AT-SPI export
//! of the same process (spike S3, `docs/plan.md` 3.2).
//!
//! GTK exports its accessible hierarchy to AT-SPI one node per accessible, in
//! order, so the two trees are aligned level by level: an order-preserving
//! alignment (Needleman-Wunsch without gap cost) maximising a pair score.
//! The score is positive only for role-compatible pairs; a matching
//! `AccessibleId`/buildable id or equal name/label raises it. Nodes left over
//! on either side stay unjoined (AT-SPI-only nodes have `ref: null`).

use super::atspi::AtNode;
use serde_json::{Map, Value, json};

/// AT-SPI roles GTK uses for roles without a closer match.
const GENERIC_ATSPI: &[&str] = &["panel", "filler", "section", "unknown", "redundant object", "invalid", "layered pane", "viewport", "scroll pane", "grouping", "paragraph"];
/// GTK roles that carry no semantics of their own.
const GENERIC_GTK: &[&str] = &["generic", "group", "widget", "presentation", "none", "section", "paragraph", "document", "application", "composite", "landmark", "region", "tab-panel", "toolbar_"];

fn expected_atspi(gtk_role: &str) -> Option<&'static [&'static str]> {
    Some(match gtk_role {
        "button" => &["push button", "toggle button", "menu item"],
        "toggle-button" => &["toggle button", "push button"],
        "checkbox" => &["check box", "check menu item", "toggle button"],
        "radio" => &["radio button", "radio menu item"],
        "switch" => &["toggle button", "switch"],
        "label" => &["label", "static", "text", "heading"],
        "window" => &["frame", "window", "dialog", "alert", "file chooser", "application"],
        "dialog" | "alert-dialog" => &["dialog", "alert", "frame", "window"],
        "text-box" | "search-box" => &["entry", "text", "password text", "terminal", "paragraph"],
        "spin-button" => &["spin button"],
        "slider" => &["slider", "scroll bar"],
        "scrollbar" => &["scroll bar", "slider"],
        "separator" => &["separator"],
        "img" | "image" => &["image", "icon", "animation"],
        "progress-bar" => &["progress bar"],
        "combo-box" => &["combo box"],
        "list" => &["list", "list box", "tree", "table"],
        "list-item" => &["list item", "table cell", "tree item"],
        "grid" | "tree-grid" => &["table", "tree table"],
        "row" => &["table row", "list item", "tree item"],
        "grid-cell" | "cell" => &["table cell", "table column header", "table row header"],
        "column-header" => &["table column header"],
        "row-header" => &["table row header"],
        "link" => &["link"],
        "menu" => &["menu", "popup menu"],
        "menu-bar" => &["menu bar"],
        "menu-item" => &["menu item"],
        "menu-item-checkbox" => &["check menu item"],
        "menu-item-radio" => &["radio menu item"],
        "tab" => &["page tab"],
        "tab-list" => &["page tab list"],
        "toolbar" => &["tool bar"],
        "tooltip" => &["tool tip"],
        "tree" => &["tree", "tree table"],
        "tree-item" => &["tree item", "list item"],
        "heading" => &["heading", "label"],
        "option" => &["list item", "menu item", "option"],
        "status" => &["status bar", "label"],
        "banner" | "navigation" | "search" | "main" | "form" => &["panel", "landmark", "filler", "section"],
        _ => return None,
    })
}

fn norm(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// GTK roles that GTK does not export to AT-SPI at all.
const UNEXPORTED_GTK: &[&str] = &["none", "presentation"];

/// Pair score; `None` when the roles are incompatible.
///
/// Finding (S3): GTK's own `GetRoleName` returns the GtkAccessibleRole name with
/// spaces (`text box`, `list item`), so equal normalised names are the strong
/// signal; the table covers roles GTK maps to a differently named AT-SPI role.
fn score(g: &Value, a: &AtNode) -> Option<i32> {
    let role = g["role"].as_str().unwrap_or("");
    if UNEXPORTED_GTK.contains(&role) {
        return None;
    }
    let arole = a.role_name.to_lowercase();
    let mut s = if norm(role) == norm(&arole) {
        6
    } else {
        match expected_atspi(role) {
            Some(list) if list.contains(&arole.as_str()) => 4,
            Some(_) | None if GENERIC_ATSPI.contains(&arole.as_str()) && !is_semantic(role) => 1,
            _ => return None,
        }
    };
    if let (Some(gid), Some(aid)) = (g["buildable_id"].as_str(), a.accessible_id.as_deref()) {
        if !gid.is_empty() && gid == aid {
            s += 20;
        }
    }
    let label = g["label"].as_str().unwrap_or("");
    if !label.is_empty() && label == a.name {
        s += 3;
    }
    Some(s)
}

/// Roles with their own AT-SPI meaning never pair with a generic container.
fn is_semantic(role: &str) -> bool {
    expected_atspi(role).is_some() && !GENERIC_GTK.contains(&role)
}

#[derive(Default, Debug, Clone)]
pub struct JoinStats {
    pub gtk_nodes: usize,
    pub atspi_nodes: usize,
    pub joined: usize,
    /// Joined pairs whose roles were an exact table match.
    pub role_exact: usize,
    pub joined_by_id: usize,
    pub unjoined_atspi: usize,
}

impl JoinStats {
    /// `join_rate` is the share of AT-SPI nodes that found their GTK counterpart;
    /// `gtk_coverage` the share of GTK accessibles that GTK exports at all
    /// (role `none`/`presentation` nodes, list-view internals and hidden content are not).
    pub fn to_json(&self) -> Value {
        let round = |f: f64| (f * 1000.0).round() / 1000.0;
        let ratio = |a: usize, b: usize| if b == 0 { 1.0 } else { a as f64 / b as f64 };
        json!({
            "gtk_nodes": self.gtk_nodes,
            "atspi_nodes": self.atspi_nodes,
            "joined": self.joined,
            "joined_by_id": self.joined_by_id,
            "role_exact": self.role_exact,
            "unjoined_atspi": self.unjoined_atspi,
            "gtk_unexported": self.gtk_nodes - self.joined,
            "join_rate": round(ratio(self.joined + 1, self.atspi_nodes)),
            "gtk_coverage": round(ratio(self.joined, self.gtk_nodes)),
        })
    }
}

enum Step {
    Pair(usize, usize),
    Gtk(usize),
    At(usize),
}

/// Order-preserving alignment of two sibling lists.
fn align(g: &[&Value], a: &[AtNode]) -> Vec<Step> {
    let (n, m) = (g.len(), a.len());
    let mut dp = vec![vec![0i32; m + 1]; n + 1];
    let sc: Vec<Vec<Option<i32>>> = g.iter().map(|gv| a.iter().map(|av| score(gv, av)).collect()).collect();
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let mut best = dp[i + 1][j].max(dp[i][j + 1]);
            if let Some(s) = sc[i][j] {
                best = best.max(dp[i + 1][j + 1] + s);
            }
            dp[i][j] = best;
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m {
            if let Some(s) = sc[i][j] {
                if dp[i][j] == dp[i + 1][j + 1] + s {
                    out.push(Step::Pair(i, j));
                    i += 1;
                    j += 1;
                    continue;
                }
            }
        }
        if i < n && (j == m || dp[i][j] == dp[i + 1][j]) {
            out.push(Step::Gtk(i));
            i += 1;
        } else {
            out.push(Step::At(j));
            j += 1;
        }
    }
    out
}

/// The AT-SPI facts attached to a node under `atspi`. Strings the app may
/// consider secret are withheld by the caller via `redact`.
pub fn atspi_json(a: &AtNode, redact: bool) -> Value {
    let mut m = Map::new();
    m.insert("bus".into(), json!(a.bus));
    m.insert("path".into(), json!(a.path));
    m.insert("role".into(), json!(a.role_name));
    m.insert("states".into(), json!(a.states));
    m.insert("interfaces".into(), json!(a.interfaces.iter().map(|i| i.rsplit('.').next().unwrap_or(i)).collect::<Vec<_>>()));
    if redact {
        m.insert("redacted".into(), json!(true));
        m.insert("has_name".into(), json!(!a.name.is_empty()));
    } else {
        m.insert("name".into(), json!(a.name));
        m.insert("description".into(), json!(a.description));
    }
    if let Some(id) = &a.accessible_id {
        m.insert("accessible_id".into(), json!(id));
    }
    if let Some(e) = &a.error {
        m.insert("error".into(), json!(e));
    }
    Value::Object(m)
}

/// `secret`: an ancestor is secret. Secrecy covers a secret widget's whole
/// subtree (plan 4.4), including AT-SPI nodes GTK exports below it.
fn unjoined(a: &AtNode, stats: &mut JoinStats, secret: bool) -> Value {
    stats.atspi_nodes += 1;
    stats.unjoined_atspi += 1;
    let kids: Vec<Value> = a.children.iter().map(|c| unjoined(c, stats, secret)).collect();
    let mut out = json!({"ref": Value::Null, "source": "atspi", "role": a.role_name, "atspi": atspi_json(a, secret), "children": kids});
    if secret {
        out["secret"] = json!(true);
    }
    out
}

fn join_node(g: &Value, a: &AtNode, stats: &mut JoinStats, by_id: bool, inherited: bool) -> Value {
    stats.gtk_nodes += 1;
    stats.atspi_nodes += 1;
    stats.joined += 1;
    if by_id {
        stats.joined_by_id += 1;
    }
    let role = g["role"].as_str().unwrap_or("");
    if expected_atspi(role).is_some_and(|l| l.contains(&a.role_name.to_lowercase().as_str())) {
        stats.role_exact += 1;
    }
    let mut out = g.as_object().cloned().unwrap_or_default();
    let secret = inherited || g["secret"].as_bool().unwrap_or(false);
    if secret {
        out.insert("secret".into(), json!(true));
    }
    out.insert("source".into(), json!("gtk+atspi"));
    out.insert("atspi".into(), atspi_json(a, secret));
    let kids = join_children(g["children"].as_array().map_or(&[][..], |v| v.as_slice()), &a.children, stats, secret);
    out.insert("children".into(), Value::Array(kids));
    Value::Object(out)
}

fn join_children(g: &[Value], a: &[AtNode], stats: &mut JoinStats, secret: bool) -> Vec<Value> {
    let gr: Vec<&Value> = g.iter().collect();
    align(&gr, a)
        .into_iter()
        .map(|s| match s {
            Step::Pair(i, j) => {
                let by_id = matches!((g[i]["buildable_id"].as_str(), a[j].accessible_id.as_deref()), (Some(x), Some(y)) if x == y);
                join_node(&g[i], &a[j], stats, by_id, secret)
            }
            Step::Gtk(i) => gtk_only(&g[i], stats, secret),
            Step::At(j) => unjoined(&a[j], stats, secret),
        })
        .collect()
}

fn gtk_only(g: &Value, stats: &mut JoinStats, inherited: bool) -> Value {
    stats.gtk_nodes += 1;
    let mut out = g.as_object().cloned().unwrap_or_default();
    let secret = inherited || g["secret"].as_bool().unwrap_or(false);
    if secret {
        out.insert("secret".into(), json!(true));
    }
    out.insert("source".into(), json!("gtk"));
    let kids: Vec<Value> = g["children"].as_array().into_iter().flatten().map(|c| gtk_only(c, stats, secret)).collect();
    out.insert("children".into(), Value::Array(kids));
    Value::Object(out)
}

/// Join the agent's `roots` (toplevels) with the application's AT-SPI window children.
pub fn join_roots(roots: &[Value], app: &AtNode) -> (Vec<Value>, JoinStats) {
    let mut stats = JoinStats::default();
    stats.atspi_nodes += 1; // the application node itself
    let out = join_children(roots, &app.children, &mut stats, false);
    (out, stats)
}

/// Join a subtree rooted at one GTK node: find its AT-SPI counterpart by
/// aligning from the application root down the same path of ancestors is not
/// possible with a subtree, so the caller joins the full toplevel trees and
/// extracts the node. Helper to find a node by ref.
pub fn find_ref<'a>(nodes: &'a [Value], r: &str) -> Option<&'a Value> {
    for n in nodes {
        if n["ref"].as_str() == Some(r) {
            return Some(n);
        }
        if let Some(f) = find_ref(n["children"].as_array().map_or(&[][..], |v| v.as_slice()), r) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(role: &str, name: &str, kids: Vec<AtNode>) -> AtNode {
        AtNode {
            bus: ":1.1".into(),
            path: format!("/{role}/{name}"),
            role: 0,
            role_name: role.into(),
            name: name.into(),
            description: String::new(),
            states: vec![],
            interfaces: vec![],
            accessible_id: None,
            children: kids,
            error: None,
        }
    }

    fn g(r: &str, role: &str, label: &str, kids: Vec<Value>) -> Value {
        json!({"ref": r, "role": role, "label": label, "children": kids})
    }

    #[test]
    fn unexported_gtk_nodes_and_extra_atspi_nodes_do_not_shift_the_pairing() {
        let gtk = vec![g(
            "w1",
            "window",
            "",
            vec![
                g("w2", "none", "", vec![]), // not exported by GTK
                g("w3", "button", "OK", vec![g("w4", "label", "OK", vec![])]),
                g("w5", "text-box", "", vec![g("w6", "none", "", vec![])]),
            ],
        )];
        let app = at(
            "application",
            "",
            vec![at(
                "window",
                "",
                vec![
                    at("button", "OK", vec![at("label", "OK", vec![])]),
                    at("text box", "", vec![]),
                    at("image", "extra", vec![]), // AT-SPI only
                ],
            )],
        );
        let (out, stats) = join_roots(&gtk, &app);
        assert_eq!(stats.joined, 4, "{stats:?}");
        assert_eq!(stats.unjoined_atspi, 1);
        let win = &out[0];
        assert_eq!(win["source"], "gtk+atspi");
        let kids = win["children"].as_array().unwrap();
        assert_eq!(kids[0]["source"], "gtk"); // role none
        assert_eq!(find_ref(&out, "w3").unwrap()["atspi"]["name"], "OK");
        assert_eq!(find_ref(&out, "w5").unwrap()["atspi"]["role"], "text box");
        assert_eq!(find_ref(&out, "w6").unwrap()["source"], "gtk");
        let extra = kids.iter().find(|k| k["source"] == "atspi").unwrap();
        assert!(extra["ref"].is_null());
    }

    #[test]
    fn secret_nodes_withhold_the_atspi_name() {
        let mut gw = g("w1", "text-box", "", vec![]);
        gw["secret"] = json!(true);
        let app = at("application", "", vec![at("text box", "Password", vec![])]);
        let (out, _) = join_roots(&[gw], &app);
        assert_eq!(out[0]["atspi"]["redacted"], true);
        assert!(out[0]["atspi"]["name"].is_null());
        assert_eq!(out[0]["atspi"]["has_name"], true);
    }

    #[test]
    fn descendants_of_secret_nodes_are_secret_on_both_sides() {
        let mut gw = g("w1", "text-box", "", vec![g("w2", "none", "", vec![g("w3", "img", "", vec![])])]);
        gw["secret"] = json!(true);
        // GTK exports a node below the entry that the agent's tree does not have.
        let app = at("application", "", vec![at("text box", "", vec![at("text", "inner-name", vec![at("label", "deep", vec![])])])]);
        let (out, stats) = join_roots(&[gw], &app);
        assert_eq!(stats.unjoined_atspi, 2, "{stats:?}");
        let s = serde_json::to_string(&out).unwrap();
        fn all_secret(n: &Value) -> bool {
            n["secret"] == true
                && n["atspi"].get("name").is_none()
                && n["atspi"].get("description").is_none()
                && n["children"].as_array().into_iter().flatten().all(all_secret)
        }
        assert!(all_secret(&out[0]), "{s}");
    }
}
