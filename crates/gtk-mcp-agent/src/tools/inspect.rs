//! `inspect`: type, ancestry, identities, state, capabilities, static tooltip
//! and related objects as refs. Works on detached widgets and any GObject ref.

use crate::agent::Agent;
use crate::registry::widget_state;
use crate::{secrecy, selector, widgets};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, InspectParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;

/// Object-valued properties that add nothing as "related" objects.
const SKIP_RELATED: &[&str] = &["parent", "root", "display", "settings", "layout-manager", "surface", "native"];

fn related(agent: &Agent, obj: &glib::Object) -> Map<String, Value> {
    let mut out = Map::new();
    for ps in obj.list_properties() {
        let name = ps.name();
        if !ps.flags().contains(glib::ParamFlags::READABLE)
            || !ps.value_type().is_a(glib::Type::OBJECT)
            || SKIP_RELATED.contains(&name)
            || name.starts_with("accessible-")
        {
            continue;
        }
        let value = obj.property_value(name);
        let Ok(Some(child)) = value.get::<Option<glib::Object>>() else { continue };
        if !child.is::<gtk::Widget>() {
            secrecy::inherit(obj, &child);
        }
        out.insert(name.to_string(), json!({"ref": agent.registry.ref_for(&child), "type": widgets::type_name(&child)}));
    }
    out
}

pub fn inspect(agent: &Rc<Agent>, p: InspectParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    let reg = &agent.registry;
    let mut m = Map::new();
    m.insert("ref".into(), json!(reg.ref_for(&obj)));
    m.insert("type".into(), json!(widgets::type_name(&obj)));
    m.insert("ancestry".into(), json!(widgets::ancestry(&obj)));
    m.insert("interfaces".into(), json!(widgets::interfaces(&obj)));
    let secret = secrecy::is_secret(&obj);
    if secret {
        m.insert("secret".into(), json!(true));
    }
    match obj.downcast_ref::<gtk::Widget>() {
        Some(w) => {
            m.insert("state".into(), json!(widget_state(w).as_str()));
            m.insert("identity".into(), widgets::identity(w));
            m.insert("role".into(), json!(widgets::role_name(w)));
            m.insert("flags".into(), Value::Object(widgets::state_map(w)));
            m.insert("capabilities".into(), json!(widgets::capabilities(w)));
            if secret {
                m.insert("text_redacted".into(), json!(true));
            } else {
                if let Some(l) = widgets::label_of(w) {
                    m.insert("label".into(), json!(l));
                }
                if let Some(t) = widgets::title_of(w) {
                    m.insert("title".into(), json!(t));
                }
                if let Some(t) = widgets::text_of(w) {
                    m.insert("text".into(), json!(t.chars().take(256).collect::<String>()));
                }
            }
            if let Some(v) = widgets::range_value(w) {
                m.insert("value".into(), v);
            }
            let tooltip = json!({
                "text": w.tooltip_text().map(|t| t.to_string()),
                "markup": w.tooltip_markup().map(|t| t.to_string()),
            });
            m.insert("tooltip".into(), tooltip);
            // Ancestry as refs, nearest first; stops at the top of the hierarchy.
            let mut chain = Vec::new();
            let mut cur = w.parent();
            while let Some(p) = cur {
                chain.push(json!({"ref": reg.ref_for(&p), "type": widgets::type_name(&p)}));
                cur = p.parent();
            }
            m.insert("parents".into(), Value::Array(chain));
            m.insert("child_count".into(), json!(widgets::child_count(w)));
        }
        None => {
            m.insert("state".into(), json!("live"));
        }
    }
    m.insert("related".into(), Value::Object(related(agent, &obj)));
    Ok(Value::Object(m))
}
