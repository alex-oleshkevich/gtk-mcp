//! `inspect_model`: paginated items of a `GListModel`, the selection, the
//! wrapper chain (selection / sort / filter / flatten / map / slice / tree)
//! and, for list-like views, the realized row widgets. Realized rows are
//! listed separately; there is no row <-> item mapping (plan 11).

use crate::agent::Agent;
use crate::json::{object_json, object_of, scalar_to_json, value_to_json};
use crate::secrecy::{self, is_type_name};
use crate::selector;
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, InspectModelParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
const MAX_CHAIN: usize = 16;
const MAX_SUMMARY_PROPS: usize = 8;
const MAX_SELECTED: u64 = 500;

fn model_of_property(obj: &glib::Object, name: &str) -> Option<gio::ListModel> {
    let ps = obj.find_property(name)?;
    if !ps.flags().contains(glib::ParamFlags::READABLE) || !ps.value_type().is_a(gio::ListModel::static_type()) {
        return None;
    }
    object_of(&obj.property_value(name))?.downcast::<gio::ListModel>().ok()
}

fn resolve_model(obj: &glib::Object) -> Result<gio::ListModel, AgentError> {
    if let Some(m) = obj.dynamic_cast_ref::<gio::ListModel>() {
        return Ok(m.clone());
    }
    if obj.is::<gtk::Widget>() {
        if let Some(ps) = obj.find_property("model") {
            if ps.value_type().is_a(gio::ListModel::static_type()) {
                return model_of_property(obj, "model").map(|m| {
                    secrecy::inherit(obj, m.upcast_ref());
                    m
                }).ok_or_else(|| {
                    AgentError::new(ErrorCode::UnsupportedOperation, format!("this {} has no model set", obj.type_().name()))
                });
            }
        }
    }
    Err(AgentError::new(
        ErrorCode::UnsupportedOperation,
        format!("{} is neither a GListModel nor a widget with a `model` property", obj.type_().name()),
    ))
}

/// Up to `MAX_SUMMARY_PROPS` readable scalar properties of a non-secret item.
fn summarize(item: &glib::Object) -> Value {
    if secrecy::is_secret(item) {
        return secrecy::redacted();
    }
    if let Some(s) = item.downcast_ref::<gtk::StringObject>() {
        return json!(s.string().as_str());
    }
    let mut m = Map::new();
    for ps in item.list_properties() {
        if m.len() >= MAX_SUMMARY_PROPS {
            break;
        }
        let f = ps.flags();
        if !f.contains(glib::ParamFlags::READABLE) || f.contains(glib::ParamFlags::DEPRECATED) {
            continue;
        }
        let v = item.property_value(ps.name());
        if let Some(j) = scalar_to_json(&ps, &v).filter(|j| !j.is_null()) {
            m.insert(ps.name().to_string(), j);
        }
    }
    Value::Object(m)
}

/// Properties of a chain layer worth reporting, by property name.
const LAYER_PROPS: &[&str] = &[
    "selected",
    "autoselect",
    "can-unselect",
    "incremental",
    "pending",
    "offset",
    "size",
    "autoexpand",
    "passthrough",
    "item-type",
];

fn layer_role(obj: &glib::Object) -> &'static str {
    if is_type_name(obj, "GtkSingleSelection") || is_type_name(obj, "GtkMultiSelection") || is_type_name(obj, "GtkNoSelection") {
        "selection"
    } else if is_type_name(obj, "GtkSortListModel") {
        "sort"
    } else if is_type_name(obj, "GtkFilterListModel") {
        "filter"
    } else if is_type_name(obj, "GtkFlattenListModel") {
        "flatten"
    } else if is_type_name(obj, "GtkMapListModel") {
        "map"
    } else if is_type_name(obj, "GtkSliceListModel") {
        "slice"
    } else if is_type_name(obj, "GtkTreeListModel") {
        "tree"
    } else if model_of_property(obj, "model").is_some() {
        "wrapper"
    } else {
        "source"
    }
}

fn chain(agent: &Agent, root: &gio::ListModel) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur: Option<(gio::ListModel, Option<glib::Object>)> = Some((root.clone(), None));
    while let Some((model, outer)) = cur.take() {
        if out.len() >= MAX_CHAIN {
            break;
        }
        let obj: glib::Object = model.clone().upcast();
        if let Some(o) = &outer {
            secrecy::inherit(o, &obj);
        }
        let role = layer_role(&obj);
        let mut m = Map::new();
        m.insert("ref".into(), json!(agent.registry.ref_for(&obj)));
        m.insert("type".into(), json!(obj.type_().name()));
        m.insert("role".into(), json!(role));
        m.insert("n_items".into(), json!(model.n_items()));
        for name in LAYER_PROPS {
            let Some(ps) = obj.find_property(name) else { continue };
            if !ps.flags().contains(glib::ParamFlags::READABLE) {
                continue;
            }
            let v = obj.property_value(name);
            if *name == "item-type" {
                // GListStore:item-type is a GType; report its name.
                if let Ok(t) = v.get::<glib::Type>() {
                    m.insert("item_type".into(), json!(t.name()));
                }
                continue;
            }
            m.insert(name.replace('-', "_"), value_to_json(agent, &obj, &ps, &v));
        }
        for (prop, key) in [("sorter", "sorter"), ("section-sorter", "section_sorter"), ("filter", "filter")] {
            let Some(ps) = obj.find_property(prop) else { continue };
            if !ps.flags().contains(glib::ParamFlags::READABLE) || !ps.value_type().is_a(glib::Type::OBJECT) {
                continue;
            }
            if let Some(child) = object_of(&obj.property_value(prop)) {
                m.insert(key.into(), object_json(agent, Some(&obj), &child));
                if prop == "filter" {
                    if let Some(sp) = child.find_property("strictness") {
                        let v = child.property_value("strictness");
                        m.insert("filter_strictness".into(), value_to_json(agent, &child, &sp, &v));
                    }
                }
            }
        }
        out.push(Value::Object(m));
        if role != "source" {
            cur = model_of_property(&obj, "model").map(|inner| (inner, Some(obj)));
        }
    }
    out
}

fn selection_of(agent: &Agent, widget: Option<&gtk::Widget>, root: &gio::ListModel) -> Value {
    let _ = agent;
    // The outermost selection model of the chain.
    let mut cur = Some(root.clone());
    let mut depth = 0;
    while let Some(m) = cur {
        if let Some(sel) = m.dynamic_cast_ref::<gtk::SelectionModel>() {
            let set = sel.selection();
            let n = set.size();
            let selected: Vec<u64> = (0..n.min(MAX_SELECTED)).map(|i| u64::from(set.nth(i as u32))).collect();
            let mut v = json!({"selected": selected, "n_selected": n});
            if n > MAX_SELECTED {
                v["truncated"] = json!(true);
            }
            return v;
        }
        depth += 1;
        cur = if depth < MAX_CHAIN { model_of_property(m.upcast_ref(), "model") } else { None };
    }
    if let Some(dd) = widget.and_then(|w| w.downcast_ref::<gtk::DropDown>()) {
        let sel = dd.selected();
        return if sel == gtk::INVALID_LIST_POSITION {
            json!({"selected": [], "n_selected": 0})
        } else {
            json!({"selected": [sel], "n_selected": 1})
        };
    }
    Value::Null
}

/// The list/grid view whose children are the realized rows.
fn row_container(w: &gtk::Widget) -> Option<gtk::Widget> {
    if w.is::<gtk::ListView>() || w.is::<gtk::GridView>() {
        return Some(w.clone());
    }
    if w.is::<gtk::ColumnView>() {
        let mut stack = vec![w.clone()];
        while let Some(cur) = stack.pop() {
            let mut c = cur.first_child();
            let mut kids = Vec::new();
            while let Some(k) = c {
                c = k.next_sibling();
                if k.is::<gtk::ListView>() {
                    return Some(k);
                }
                kids.push(k);
            }
            stack.extend(kids.into_iter().rev());
        }
    }
    None
}

fn realized_rows(agent: &Agent, w: &gtk::Widget) -> Value {
    if !(w.is::<gtk::ListView>() || w.is::<gtk::GridView>() || w.is::<gtk::ColumnView>()) {
        return Value::Null;
    }
    let mut rows = Vec::new();
    if let Some(container) = row_container(w) {
        let mut c = container.first_child();
        while let Some(k) = c {
            c = k.next_sibling();
            if rows.len() >= MAX_LIMIT as usize {
                break;
            }
            rows.push(json!(agent.registry.ref_for(&k)));
        }
    }
    Value::Array(rows)
}

pub fn inspect_model(agent: &Rc<Agent>, p: InspectModelParams) -> Result<Value, AgentError> {
    let obj = selector::resolve_object(agent, &p.target)?;
    let model = resolve_model(&obj)?;
    let model_obj: glib::Object = model.clone().upcast();
    let offset = p.offset.unwrap_or(0);
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let n_items = model.n_items();

    let mut items = Vec::new();
    for index in offset..offset.saturating_add(limit).min(n_items) {
        let Some(item) = model.item(index) else { continue };
        secrecy::inherit(&model_obj, &item);
        items.push(json!({
            "index": index,
            "ref": agent.registry.ref_for(&item),
            "type": item.type_().name(),
            "summary": summarize(&item),
        }));
    }

    let widget = obj.downcast_ref::<gtk::Widget>();
    Ok(json!({
        "target": agent.registry.ref_for(&obj),
        "model": {"ref": agent.registry.ref_for(&model_obj), "type": model_obj.type_().name()},
        "n_items": n_items,
        "item_type": model.item_type().name(),
        "offset": offset,
        "limit": limit,
        "items": items,
        "selection": selection_of(agent, widget, &model),
        "chain": chain(agent, &model),
        "realized_rows": widget.map_or(Value::Null, |w| realized_rows(agent, w)),
    }))
}
