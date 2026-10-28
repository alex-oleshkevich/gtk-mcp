//! `audit_accessibility` rules over the joined accessible tree.
//!
//! GTK rules need only the agent's data. AT-SPI rules need the join and are
//! reported as skipped (with the reason) when the source is off or the app is
//! not on the a11y bus.

use super::Source;
use gtk_mcp_protocol::{AgentError, ErrorCode};
use serde_json::{Value, json};

/// GTK roles of widgets a user operates.
const INTERACTIVE: &[&str] = &[
    "button", "toggle-button", "checkbox", "radio", "switch", "link", "text-box", "search-box", "spin-button", "slider",
    "combo-box", "menu-item", "menu-item-checkbox", "menu-item-radio", "tab", "option",
];

/// Minimum target size in px (WCAG 2.2 SC 2.5.8).
const MIN_TARGET: i64 = 24;

struct Rule {
    name: &'static str,
    source: &'static str,
    severity: &'static str,
    summary: &'static str,
}

const RULES: &[Rule] = &[
    Rule { name: "interactive_not_focusable", source: "gtk", severity: "warning", summary: "visible, sensitive interactive widget that keyboard focus cannot reach" },
    Rule { name: "icon_button_no_label", source: "gtk", severity: "warning", summary: "icon-only button without label or tooltip" },
    Rule { name: "clickable_without_role", source: "gtk", severity: "warning", summary: "clickable widget (click gesture) whose accessible role is not interactive" },
    Rule { name: "small_target", source: "gtk", severity: "info", summary: "interactive widget smaller than 24x24 px" },
    Rule { name: "window_untitled", source: "gtk", severity: "warning", summary: "window without a title" },
    Rule { name: "missing_name", source: "atspi", severity: "error", summary: "interactive widget with an empty AT-SPI accessible name" },
    Rule { name: "image_without_text", source: "atspi", severity: "warning", summary: "image with no accessible name outside a named control" },
    Rule { name: "interactive_not_exported", source: "atspi", severity: "warning", summary: "interactive widget with no AT-SPI node" },
];

fn is_interactive(n: &Value) -> bool {
    n["role"].as_str().is_some_and(|r| INTERACTIVE.contains(&r))
}

/// The node carries an application-added press-based gesture controller,
/// beyond the widget type's GTK defaults, so it can be operated with pointer
/// clicks even when its accessible role is not interactive (tray icons).
fn is_clickable(n: &Value) -> bool {
    n["clickable"].as_bool().unwrap_or(false)
}

fn has_focusable_descendant(n: &Value) -> bool {
    n["children"].as_array().into_iter().flatten().any(|c| {
        c["platform_state"]["focusable"].as_bool().unwrap_or(false) || has_focusable_descendant(c)
    })
}

fn at_name_present(n: &Value) -> Option<bool> {
    let at = &n["atspi"];
    if !at.is_object() {
        return None;
    }
    if at["redacted"].as_bool().unwrap_or(false) {
        return Some(at["has_name"].as_bool().unwrap_or(false));
    }
    Some(!at["name"].as_str().unwrap_or("").trim().is_empty())
}

struct Ctx<'a> {
    findings: Vec<Value>,
    enabled: &'a dyn Fn(&str) -> bool,
    atspi: bool,
}

impl Ctx<'_> {
    fn report(&mut self, rule: &str, n: &Value, message: String, evidence: Value) {
        if !(self.enabled)(rule) {
            return;
        }
        let r = RULES.iter().find(|r| r.name == rule).expect("known rule");
        self.findings.push(json!({
            "rule": rule,
            "severity": r.severity,
            "source": r.source,
            "ref": n["ref"],
            "type": n["type"],
            "role": n["role"],
            "message": message,
            "evidence": evidence,
        }));
    }
}

fn evidence(n: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for k in ["label", "tooltip", "icon_name", "bounds", "buildable_id", "automation_id"] {
        if !n[k].is_null() {
            m.insert(k.into(), n[k].clone());
        }
    }
    if n["atspi"].is_object() {
        m.insert("atspi_role".into(), n["atspi"]["role"].clone());
        m.insert("atspi_name".into(), n["atspi"]["name"].clone());
    }
    Value::Object(m)
}

fn walk(n: &Value, parent_type: &str, named_control_above: bool, cx: &mut Ctx) {
    let ty = n["type"].as_str().unwrap_or("");
    let role = n["role"].as_str().unwrap_or("");
    let mapped = n["mapped"].as_bool().unwrap_or(false);
    let sensitive = n["sensitive"].as_bool().unwrap_or(true);
    let secret = n["secret"].as_bool().unwrap_or(false);
    // The inner GtkText of an entry is part of the entry's own accessible.
    let inner = ty == "GtkText" && !parent_type.is_empty() && parent_type != "GtkText";
    // Clickable widgets (click gesture) get the interactive checks even when
    // their accessible role is not interactive (tray icons).
    let clickable = is_clickable(n) && !inner;
    let role_interactive = is_interactive(n);
    let interactive = (role_interactive || clickable) && !inner;
    let joined = n["atspi"].is_object();
    let label_empty = n["label"].as_str().unwrap_or("").is_empty();

    if mapped && !inner {
        if interactive && sensitive && !n["platform_state"]["focusable"].as_bool().unwrap_or(false) && !has_focusable_descendant(n) {
            cx.report(
                "interactive_not_focusable",
                n,
                format!("{ty} ({role}) is interactive but not keyboard focusable"),
                evidence(n),
            );
        }
        if role == "button" && n["icon_name"].is_string() && label_empty && n["tooltip"].is_null() && !secret {
            cx.report(
                "icon_button_no_label",
                n,
                format!("{ty} shows only the icon '{}' and has no label or tooltip to derive a name from", n["icon_name"].as_str().unwrap_or("")),
                evidence(n),
            );
        }
        if clickable && !role_interactive && !secret {
            cx.report(
                "clickable_without_role",
                n,
                format!("{ty} reacts to clicks (click gesture) but its role '{role}' is not interactive; give it role 'button' and a label"),
                evidence(n),
            );
        }
        if interactive {
            let (w, h) = (n["bounds"]["width"].as_i64(), n["bounds"]["height"].as_i64());
            if let (Some(w), Some(h)) = (w, h) {
                if w.min(h) < MIN_TARGET && w > 0 && h > 0 {
                    cx.report("small_target", n, format!("{ty} is {w}x{h} px; the minimum target size is {MIN_TARGET}x{MIN_TARGET}"), evidence(n));
                }
            }
        }
        if role == "window" && label_empty && ty != "GtkPopover" && n["children"].as_array().is_some_and(|c| !c.is_empty()) {
            cx.report("window_untitled", n, format!("{ty} has no title"), evidence(n));
        }
    }
    if cx.atspi && mapped && !inner {
        if interactive && !joined {
            cx.report("interactive_not_exported", n, format!("{ty} ({role}) has no node on the AT-SPI bus"), evidence(n));
        }
        if interactive && joined && at_name_present(n) == Some(false) {
            cx.report("missing_name", n, format!("{ty} ({role}) has an empty accessible name; assistive technology announces only its role"), evidence(n));
        }
        if matches!(role, "img" | "image") && joined && at_name_present(n) == Some(false) && !named_control_above {
            cx.report("image_without_text", n, format!("{ty} has no accessible name; label it or mark it presentational"), evidence(n));
        }
    }
    let named_here = named_control_above || (interactive && at_name_present(n) == Some(true));
    for c in n["children"].as_array().into_iter().flatten() {
        walk(c, ty, named_here, cx);
    }
}

pub(crate) fn run(roots: &[Value], source: &Source, only: Option<&[String]>) -> Result<Value, AgentError> {
    if let Some(only) = only {
        for r in only {
            if !RULES.iter().any(|x| x.name == r) {
                return Err(AgentError::new(
                    ErrorCode::InvalidParams,
                    format!("unknown rule '{r}' (rules: {})", RULES.iter().map(|r| r.name).collect::<Vec<_>>().join(", ")),
                ));
            }
        }
    }
    let enabled = |r: &str| only.is_none_or(|o| o.iter().any(|x| x == r));
    let mut cx = Ctx { findings: Vec::new(), enabled: &enabled, atspi: source.available() };
    for r in roots {
        walk(r, "", false, &mut cx);
    }
    let rules: Vec<Value> = RULES
        .iter()
        .filter(|r| enabled(r.name))
        .map(|r| {
            let count = cx.findings.iter().filter(|f| f["rule"] == r.name).count();
            if r.source == "atspi" && !source.available() {
                json!({"name": r.name, "source": r.source, "summary": r.summary, "status": "skipped", "reason": source.reason()})
            } else {
                json!({"name": r.name, "source": r.source, "summary": r.summary, "status": "ran", "findings": count})
            }
        })
        .collect();
    let findings = cx.findings;
    Ok(json!({"findings": findings, "finding_count": findings.len(), "rules": rules}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(ok: bool) -> Source {
        Source {
            status: if ok { Ok(Default::default()) } else { Err("AT-SPI is off: start the broker with --allow-atspi".into()) },
            atspi: None,
        }
    }

    fn button(name: Option<&str>) -> Value {
        let mut n = json!({"ref": "w1", "type": "GtkButton", "role": "button", "mapped": true, "sensitive": true,
            "icon_name": "edit-delete-symbolic", "platform_state": {"focusable": true},
            "bounds": {"x": 0, "y": 0, "width": 32, "height": 32}, "children": []});
        if let Some(name) = name {
            n["atspi"] = json!({"name": name, "role": "button"});
        }
        n
    }

    /// A tray icon: clickable (click gesture) but its role is not interactive.
    fn clickable_image() -> Value {
        json!({"ref": "w2", "type": "GtkImage", "role": "img", "mapped": true, "sensitive": true,
            "clickable": true, "icon_name": "battery-good-symbolic", "platform_state": {"focusable": false},
            "bounds": {"x": 0, "y": 0, "width": 16, "height": 16}, "children": []})
    }

    #[test]
    fn clickable_widget_without_interactive_role_is_a_finding() {
        let r = run(&[clickable_image()], &src(false), Some(&["clickable_without_role".to_string()])).unwrap();
        let findings = r["findings"].as_array().unwrap();
        let f = findings
            .iter()
            .find(|f| f["rule"] == "clickable_without_role")
            .unwrap_or_else(|| panic!("a clickable_without_role finding: {r}"));
        assert_eq!(f["ref"], "w2");
        assert_eq!(f["severity"], "warning");
        assert_eq!(f["source"], "gtk");
        assert!(f["message"].as_str().unwrap().contains("'button'"), "{f}");
    }

    #[test]
    fn clickable_widgets_get_the_interactive_checks() {
        let rules = ["interactive_not_focusable".to_string(), "small_target".to_string()];
        let r = run(&[clickable_image()], &src(false), Some(&rules)).unwrap();
        let findings = r["findings"].as_array().unwrap();
        assert!(findings.iter().any(|f| f["rule"] == "interactive_not_focusable" && f["ref"] == "w2"), "{r}");
        assert!(findings.iter().any(|f| f["rule"] == "small_target" && f["ref"] == "w2"), "{r}");
    }

    #[test]
    fn interactive_role_or_no_click_fact_is_not_a_clickable_finding() {
        // A real button has an interactive role: no clickable_without_role finding.
        let r = run(&[button(None)], &src(false), Some(&["clickable_without_role".to_string()])).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty(), "{r}");
        // A non-clickable image is not interactive at all: nothing reported.
        let mut img = clickable_image();
        img.as_object_mut().unwrap().remove("clickable");
        let r = run(&[img], &src(false), None).unwrap();
        assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["ref"] == "w2"), "{r}");
    }

    #[test]
    fn atspi_rules_are_skipped_without_the_source() {
        let r = run(&[button(None)], &src(false), None).unwrap();
        let rules = r["rules"].as_array().unwrap();
        assert!(rules.iter().filter(|x| x["source"] == "atspi").all(|x| x["status"] == "skipped"));
        assert_eq!(r["findings"].as_array().unwrap().iter().filter(|f| f["source"] == "atspi").count(), 0);
        assert!(r["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "icon_button_no_label"));
    }

    #[test]
    fn empty_atspi_name_is_a_finding_and_a_name_is_not() {
        let bad = run(&[button(Some(""))], &src(true), None).unwrap();
        assert!(bad["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "missing_name"));
        let good = run(&[button(Some("Delete"))], &src(true), Some(&["missing_name".to_string()])).unwrap();
        assert!(good["findings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unknown_rule_is_rejected() {
        assert!(run(&[], &src(true), Some(&["nope".to_string()])).is_err());
    }
}
