//! Trims the generated input schemas before they are listed. Clients send
//! each schema verbatim to the model when they load a tool, so boilerplate
//! costs context. Arguments are still validated by serde on the server, which
//! these edits do not touch.

use serde_json::{Map, Value};

/// `format` values schemars adds to numeric types; `minimum`/`maximum` already carry the range.
const NUMERIC_FORMATS: &[&str] = &[
    "uint", "uint8", "uint16", "uint32", "uint64", "int", "int8", "int16", "int32", "int64", "float", "double",
];

const DEFS_PREFIX: &str = "#/$defs/";

/// Drops `$schema`, schema-level `title`s and numeric `format`s; lets optional
/// properties omit the `null` alternative (the server still accepts it);
/// inlines `$defs` that are cheaper inline than referenced.
pub fn slim(mut schema: Map<String, Value>) -> Map<String, Value> {
    schema.remove("$schema");
    let mut root = Value::Object(schema);
    clean(&mut root);
    inline_defs(&mut root);
    clean(&mut root); // inlined bodies get the same treatment
    match root {
        Value::Object(m) => m,
        _ => unreachable!("a schema object stays an object"),
    }
}

/// Cleans a schema node and everything below it.
fn clean(node: &mut Value) {
    match node {
        Value::Object(m) => {
            m.remove("title");
            if m.get("format").and_then(Value::as_str).is_some_and(|f| NUMERIC_FORMATS.contains(&f)) {
                m.remove("format");
            }
            if m.contains_key("const") {
                m.remove("type");
            }
            if m.get("default").is_some_and(|d| d == "") {
                m.remove("default");
            }
            collapse_const_variants(m);
            if m.get("additionalProperties") == Some(&Value::Bool(false)) {
                m.remove("additionalProperties");
            }
            let required: Vec<String> = m
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            for (k, v) in m.iter_mut() {
                match k.as_str() {
                    "properties" => {
                        if let Value::Object(props) = v {
                            for (name, p) in props.iter_mut() {
                                clean(p);
                                if !required.contains(name) {
                                    drop_null(p);
                                }
                                if *p == Value::Bool(true) {
                                    *p = Value::Object(Map::new()); // some clients reject boolean schemas
                                }
                                compact_flat_object(p);
                            }
                        }
                    }
                    "$defs" => {
                        if let Value::Object(defs) = v {
                            for d in defs.values_mut() {
                                clean(d);
                                compact_flat_object(d);
                            }
                        }
                    }
                    "items" => {
                        clean(v);
                        compact_flat_object(v);
                    }
                    "additionalProperties" | "not" | "if" | "then" | "else" => clean(v),
                    "oneOf" | "anyOf" | "allOf" | "prefixItems" => {
                        if let Value::Array(a) = v {
                            a.iter_mut().for_each(clean);
                        }
                    }
                    _ => {}
                }
            }
            collapse_tagged_variants(m);
            collapse_single_key_variants(m);
        }
        Value::Array(a) => a.iter_mut().for_each(clean),
        _ => {}
    }
}

/// `oneOf` of objects sharing one string-`const` discriminator (an internally
/// tagged enum) becomes one object: the tag as an `enum`, the other keys
/// merged, and a line saying which variant takes which keys.
fn collapse_tagged_variants(m: &mut Map<String, Value>) {
    let Some(Value::Array(variants)) = m.get("oneOf") else { return };
    let objects: Vec<&Map<String, Value>> = variants.iter().filter_map(Value::as_object).collect();
    if objects.len() != variants.len() || objects.len() < 2 {
        return;
    }
    let props_of = |o: &Map<String, Value>| o.get("properties").and_then(Value::as_object).cloned();
    let Some(first) = props_of(objects[0]) else { return };
    // The tag: a property that is a string `const` in every variant.
    let Some(tag) = first.iter().find_map(|(k, v)| {
        let ok = objects.iter().all(|o| props_of(o).is_some_and(|p| p.get(k).is_some_and(|v| v.get("const").is_some_and(Value::is_string))));
        (ok && v.get("const").is_some()).then(|| k.clone())
    }) else {
        return;
    };
    let mut merged = Map::new();
    let mut tags = Vec::new();
    let mut notes = Vec::new();
    for o in &objects {
        if o.keys().any(|k| !matches!(k.as_str(), "properties" | "required" | "type" | "description")) {
            return;
        }
        let Some(props) = props_of(o) else { return };
        let required: Vec<&str> = o.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let name = props[&tag]["const"].clone();
        let mut args = Vec::new();
        for (k, v) in &props {
            if *k == tag {
                continue;
            }
            match merged.get(k) {
                // The same key typed differently per variant: accept anything.
                Some(existing) if existing != v => {
                    merged.insert(k.clone(), Value::Bool(true));
                }
                Some(_) => {}
                None => {
                    merged.insert(k.clone(), v.clone());
                }
            }
            args.push(if required.contains(&k.as_str()) { k.clone() } else { format!("{k}?") });
        }
        let detail = o.get("description").and_then(Value::as_str).map(|d| format!(" - {}", d.trim_end_matches('.')));
        if !args.is_empty() || detail.is_some() {
            let args = if args.is_empty() { String::new() } else { format!("({})", args.join(", ")) };
            notes.push(format!("{}{args}{}", name.as_str().unwrap_or_default(), detail.unwrap_or_default()));
        }
        tags.push(name);
    }
    m.remove("oneOf");
    let mut props = m.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    props.insert(tag.clone(), serde_json::json!({ "enum": tags }));
    for (k, v) in merged {
        props.entry(k).or_insert(v);
    }
    m.insert("properties".into(), Value::Object(props));
    m.insert("type".into(), "object".into());
    let mut required: Vec<Value> = m.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
    required.push(tag.clone().into());
    m.insert("required".into(), Value::Array(required));
    if !notes.is_empty() {
        let line = format!("Per {tag}: {}.", notes.join("; "));
        let description = match m.get("description").and_then(Value::as_str) {
            Some(d) => format!("{d} {line}"),
            None => line,
        };
        m.insert("description".into(), description.into());
    }
}

/// `oneOf` of string `const`s (an enum with per-variant docs) becomes an `enum`.
fn collapse_const_variants(m: &mut Map<String, Value>) {
    let Some(Value::Array(variants)) = m.get("oneOf") else { return };
    let mut names = Vec::with_capacity(variants.len());
    for v in variants {
        let Some(o) = v.as_object() else { return };
        let Some(c) = o.get("const").filter(|c| c.is_string()) else { return };
        if o.keys().any(|k| !matches!(k.as_str(), "const" | "description" | "type")) {
            return;
        }
        names.push(c.clone());
    }
    m.remove("oneOf");
    m.insert("type".into(), "string".into());
    m.insert("enum".into(), Value::Array(names));
}

/// `oneOf` of objects that each carry exactly one, distinct, required key
/// (an externally tagged enum) becomes one object with all keys optional.
fn collapse_single_key_variants(m: &mut Map<String, Value>) {
    let Some(Value::Array(variants)) = m.get("oneOf") else { return };
    let mut merged = Map::new();
    for v in variants {
        let Some(o) = v.as_object() else { return };
        let Some(Value::Object(props)) = o.get("properties") else { return };
        let single = props.len() == 1 && o.get("required").and_then(Value::as_array).is_some_and(|r| r.len() == 1 && props.contains_key(r[0].as_str().unwrap_or("")));
        let extra = o.keys().any(|k| !matches!(k.as_str(), "properties" | "required" | "type" | "description"));
        let (Some((key, prop)), false, true) = (props.iter().next(), extra, single) else { return };
        if merged.contains_key(key) {
            return;
        }
        let mut prop = prop.clone();
        if let (Some(d), Some(p)) = (o.get("description"), prop.as_object_mut()) {
            p.entry("description").or_insert_with(|| d.clone());
        }
        merged.insert(key.clone(), prop);
    }
    m.remove("oneOf");
    m.insert("type".into(), "object".into());
    m.insert("properties".into(), Value::Object(merged));
    let note = "Exactly one key.";
    let description = match m.get("description").and_then(Value::as_str) {
        Some(d) => format!("{d} {note}"),
        None => note.to_owned(),
    };
    m.insert("description".into(), description.into());
}

/// In a nested object, three or more optional plain scalar properties (and
/// any already-compacted object properties) are replaced by one line naming
/// them; their per-key schemas cost more context than the names and types say.
fn compact_flat_object(p: &mut Value) {
    let Value::Object(m) = p else { return };
    let required: Vec<&str> = m.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let Some(Value::Object(props)) = m.get("properties") else { return };
    let mut by_type: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut folded = Vec::new();
    let mut nested = Vec::new();
    for (name, schema) in props {
        let Some(o) = schema.as_object() else { continue };
        if required.contains(&name.as_str()) {
            continue;
        }
        let Some(t) = o.get("type").and_then(Value::as_str) else { continue };
        if matches!(t, "string" | "boolean" | "integer" | "number") && o.keys().all(|k| matches!(k.as_str(), "type" | "minimum" | "maximum")) {
            folded.push(name.clone());
            match by_type.iter_mut().find(|(bt, _)| *bt == t) {
                Some((_, names)) => names.push(name),
                None => by_type.push((t, vec![name])),
            }
        } else if let (true, Some(d)) = (t == "object" && o.len() == 2, o.get("description").and_then(Value::as_str)) {
            let d = d.trim_end_matches('.');
            let d = d.strip_prefix("Keys: ").unwrap_or(d).replace(". Keys: ", ": ");
            folded.push(name.clone());
            nested.push(format!("{name} {{{d}}}"));
        }
    }
    let previous = m.get("description").and_then(Value::as_str).filter(|d| d.starts_with("Keys: ")).map(str::to_owned);
    let scalars = by_type.iter().map(|(_, n)| n.len()).sum::<usize>();
    if scalars < 3 && !(scalars + nested.len() > 0 && previous.is_some()) {
        return;
    }
    let mut parts: Vec<String> = by_type.iter().map(|(t, names)| format!("{t} {}", names.join(", "))).collect();
    parts.extend(nested);
    let description = match (previous, m.get("description").and_then(Value::as_str)) {
        (Some(prev), _) => format!("{}; {}.", prev.trim_end_matches('.'), parts.join("; ")),
        (None, Some(d)) => format!("{d} Keys: {}.", parts.join("; ")),
        (None, None) => format!("Keys: {}.", parts.join("; ")),
    };
    if let Some(Value::Object(props)) = m.get_mut("properties") {
        for name in &folded {
            props.remove(name);
        }
        if props.is_empty() {
            m.remove("properties");
        }
    }
    m.insert("description".into(), description.into());
}

/// Removes the `null` alternative from an optional property's schema.
fn drop_null(p: &mut Value) {
    let Value::Object(m) = p else { return };
    if let Some(Value::Array(types)) = m.get_mut("type") {
        types.retain(|t| t != "null");
        if types.len() == 1 {
            let only = types.remove(0);
            m.insert("type".into(), only);
        }
        return;
    }
    let is_null = |v: &Value| v.as_object().is_some_and(|o| o.len() == 1 && o.get("type").is_some_and(|t| t == "null"));
    if m.get("anyOf").and_then(Value::as_array).is_some_and(|alts| alts.len() == 2 && alts.iter().any(is_null)) {
        let Some(Value::Array(mut alts)) = m.remove("anyOf") else { return };
        alts.retain(|a| !is_null(a));
        if let Some(Value::Object(inner)) = alts.pop() {
            for (k, v) in inner {
                m.entry(k).or_insert(v);
            }
        }
    }
}

fn ref_name(v: &Value) -> Option<&str> {
    v.as_object()?.get("$ref")?.as_str()?.strip_prefix(DEFS_PREFIX)
}

fn count_refs(v: &Value, name: &str, n: &mut usize) {
    match v {
        Value::Object(m) => {
            if ref_name(v) == Some(name) {
                *n += 1;
            }
            m.values().for_each(|c| count_refs(c, name, n));
        }
        Value::Array(a) => a.iter().for_each(|c| count_refs(c, name, n)),
        _ => {}
    }
}

/// Replaces `{"$ref": "#/$defs/Name", ..siblings}` by the definition body.
fn substitute(v: &mut Value, name: &str, body: &Value) {
    match v {
        Value::Object(m) => {
            if m.get("$ref").and_then(Value::as_str).and_then(|r| r.strip_prefix(DEFS_PREFIX)) == Some(name) {
                m.remove("$ref");
                if let Value::Object(b) = body {
                    for (k, val) in b {
                        m.entry(k.clone()).or_insert_with(|| val.clone());
                    }
                }
            }
            m.values_mut().for_each(|c| substitute(c, name, body));
        }
        Value::Array(a) => a.iter_mut().for_each(|c| substitute(c, name, body)),
        _ => {}
    }
}

fn len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

/// Inlines definitions that are used once, or whose copies cost less than the
/// `$ref`s plus the `$defs` entry; drops definitions nothing references.
fn inline_defs(root: &mut Value) {
    loop {
        let Some(defs) = root.get("$defs").and_then(Value::as_object) else { return };
        let mut pick = None;
        for (name, body) in defs {
            let mut total = 0;
            count_refs(root, name, &mut total);
            let mut own = 0;
            count_refs(body, name, &mut own);
            if own > 0 {
                continue; // recursive
            }
            let size = len(body);
            let ref_cost = DEFS_PREFIX.len() + name.len() + 10; // {"$ref":"…"}
            let keep_cost = total * ref_cost + name.len() + 4 + size;
            if total == 0 || total * size <= keep_cost {
                pick = Some((name.clone(), body.clone()));
                break;
            }
        }
        let Some((name, body)) = pick else { break };
        if let Some(Value::Object(defs)) = root.get_mut("$defs") {
            defs.remove(&name);
        }
        substitute(root, &name, &body);
    }
    if root.get("$defs").and_then(Value::as_object).is_some_and(Map::is_empty) {
        root.as_object_mut().map(|m| m.remove("$defs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(v: Value) -> Value {
        let Value::Object(m) = v else { panic!() };
        Value::Object(slim(m))
    }

    #[test]
    fn keeps_a_property_named_title_and_required_nulls() {
        let out = run(json!({
            "$schema": "x", "title": "T", "type": "object",
            "properties": {
                "title": {"type": "string", "title": "noise"},
                "n": {"type": ["integer", "null"], "format": "uint32", "minimum": 0},
                "req": {"type": ["string", "null"]},
            },
            "required": ["req"],
        }));
        assert!(out.get("$schema").is_none() && out.get("title").is_none());
        assert_eq!(out["properties"]["title"], json!({"type": "string"}));
        assert_eq!(out["properties"]["n"], json!({"type": "integer", "minimum": 0}));
        assert_eq!(out["properties"]["req"]["type"], json!(["string", "null"]));
    }

    #[test]
    fn inlines_single_use_defs_and_keeps_recursive_ones() {
        let out = run(json!({
            "type": "object",
            "properties": {
                "a": {"anyOf": [{"$ref": "#/$defs/One"}, {"type": "null"}], "description": "d"},
                "b": {"$ref": "#/$defs/Rec"},
            },
            "$defs": {
                "One": {"type": "string"},
                "Rec": {"type": "object", "properties": {"c": {"$ref": "#/$defs/Rec"}}},
            },
        }));
        assert_eq!(out["properties"]["a"], json!({"type": "string", "description": "d"}));
        assert_eq!(out["properties"]["b"], json!({"$ref": "#/$defs/Rec"}));
        assert!(out["$defs"].get("One").is_none() && out["$defs"].get("Rec").is_some());
    }
}
