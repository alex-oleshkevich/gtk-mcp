//! The `app` toolset (`app_open`, `inspect_gsettings`, `set_gsetting`,
//! `list_resources`, `read_resource`) and `object_stats` (`docs/tools.md`).

use crate::agent::Agent;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gio::prelude::*;
use glib::translate::IntoGlib;
use glib::variant::Variant;
use glib::{VariantClass, VariantTy, VariantType};
use gtk_mcp_protocol::{
    AgentError, AppOpenParams, DEFAULT_RESOURCE_READ, ErrorCode, InspectGsettingsParams, ListResourcesParams,
    MAX_RESOURCE_READ, ObjectStatsParams, ReadResourceParams, ResourceEncoding, SetGsettingParams,
};
use serde_json::{Map, Value, json};
use std::rc::Rc;

fn err(code: ErrorCode, msg: impl Into<String>) -> AgentError {
    AgentError::new(code, msg)
}

// ---- app_open -------------------------------------------------------------

pub fn app_open(agent: &Rc<Agent>, p: AppOpenParams) -> Result<Value, AgentError> {
    let files = p.files.unwrap_or_default();
    if p.activate == Some(true) && !files.is_empty() {
        return Err(AgentError::invalid_params("`activate` and `files` are mutually exclusive"));
    }
    let app = &agent.app;
    if !app.is_registered() {
        return Err(err(ErrorCode::Unavailable, "the application is not registered yet"));
    }
    if files.is_empty() {
        if p.activate == Some(false) {
            return Err(AgentError::invalid_params("nothing to do: pass `files` or `activate`"));
        }
        app.activate();
        return Ok(json!({ "action": "activate" }));
    }
    if !app.flags().contains(gio::ApplicationFlags::HANDLES_OPEN) {
        return Err(err(
            ErrorCode::UnsupportedOperation,
            "the application was not created with G_APPLICATION_HANDLES_OPEN; it cannot open files",
        ));
    }
    let files: Vec<gio::File> = files
        .iter()
        .map(|f| if f.contains("://") { gio::File::for_uri(f) } else { gio::File::for_path(f) })
        .collect();
    let hint = p.hint.unwrap_or_default();
    app.open(&files, &hint);
    Ok(json!({
        "action": "open",
        "files": files.iter().map(|f| f.uri().to_string()).collect::<Vec<_>>(),
        "hint": hint,
    }))
}

// ---- GSettings ------------------------------------------------------------

/// Key names containing these are reported `{"redacted": true}`: GSettings has
/// no secrecy flag, apps keep credentials out of it, but a key named like one
/// must not leak through a debugging tool (plan 4.4 spirit).
fn key_is_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["password", "passwd", "passphrase", "secret", "token", "credential", "api-key", "apikey", "private-key"]
        .iter()
        .any(|w| n.contains(w))
}

fn source() -> Result<gio::SettingsSchemaSource, AgentError> {
    gio::SettingsSchemaSource::default()
        .ok_or_else(|| err(ErrorCode::Unavailable, "no GSettings schema source is installed (GSETTINGS_SCHEMA_DIR / XDG_DATA_DIRS)"))
}

pub fn variant_to_json(v: &Variant) -> Value {
    match v.classify() {
        VariantClass::Boolean => json!(v.get::<bool>()),
        VariantClass::Byte => json!(v.get::<u8>()),
        VariantClass::Int16 => json!(v.get::<i16>()),
        VariantClass::Uint16 => json!(v.get::<u16>()),
        VariantClass::Int32 => json!(v.get::<i32>()),
        VariantClass::Uint32 => json!(v.get::<u32>()),
        VariantClass::Int64 => json!(v.get::<i64>()),
        VariantClass::Uint64 => json!(v.get::<u64>()),
        VariantClass::Handle => json!(v.get::<i32>()),
        VariantClass::Double => {
            let d = v.get::<f64>().unwrap_or(f64::NAN);
            if d.is_finite() { json!(d) } else { json!(d.to_string()) }
        }
        VariantClass::String | VariantClass::ObjectPath | VariantClass::Signature => {
            json!(v.str().unwrap_or_default())
        }
        VariantClass::Variant => match v.as_variant() {
            Some(inner) => json!({ "type": inner.type_().as_str(), "value": variant_to_json(&inner) }),
            None => Value::Null,
        },
        VariantClass::Maybe => v.as_maybe().map_or(Value::Null, |inner| variant_to_json(&inner)),
        VariantClass::Array => {
            let elem = v.type_().element();
            if elem.is_dict_entry() && is_string_like(elem.key()) {
                let mut m = Map::new();
                for i in 0..v.n_children() {
                    let e = v.child_value(i);
                    m.insert(e.child_value(0).str().unwrap_or_default().to_owned(), variant_to_json(&e.child_value(1)));
                }
                Value::Object(m)
            } else {
                Value::Array((0..v.n_children()).map(|i| variant_to_json(&v.child_value(i))).collect())
            }
        }
        VariantClass::Tuple | VariantClass::DictEntry => {
            Value::Array((0..v.n_children()).map(|i| variant_to_json(&v.child_value(i))).collect())
        }
        _ => json!(v.print(true).to_string()),
    }
}

fn is_string_like(t: &VariantTy) -> bool {
    matches!(t.as_str(), "s" | "o" | "g")
}

fn json_to_variant(ty: &VariantTy, v: &Value) -> Result<Variant, String> {
    let want = |what: &str| format!("expected {what} for GVariant type '{}', got {}", ty.as_str(), v);
    fn int(v: &Value, lo: i128, hi: i128) -> Option<i128> {
        let n = v.as_i64().map(i128::from).or_else(|| v.as_u64().map(i128::from))?;
        (lo..=hi).contains(&n).then_some(n)
    }
    match ty.as_str() {
        "b" => v.as_bool().map(|b| b.to_variant()).ok_or_else(|| want("a boolean")),
        "y" => int(v, 0, u8::MAX.into()).map(|n| (n as u8).to_variant()).ok_or_else(|| want("an integer 0..=255")),
        "n" => int(v, i16::MIN.into(), i16::MAX.into()).map(|n| (n as i16).to_variant()).ok_or_else(|| want("an int16")),
        "q" => int(v, 0, u16::MAX.into()).map(|n| (n as u16).to_variant()).ok_or_else(|| want("a uint16")),
        "i" => int(v, i32::MIN.into(), i32::MAX.into()).map(|n| (n as i32).to_variant()).ok_or_else(|| want("an int32")),
        "u" => int(v, 0, u32::MAX.into()).map(|n| (n as u32).to_variant()).ok_or_else(|| want("a uint32")),
        "x" => int(v, i64::MIN.into(), i64::MAX.into()).map(|n| (n as i64).to_variant()).ok_or_else(|| want("an int64")),
        "t" => int(v, 0, u64::MAX.into()).map(|n| (n as u64).to_variant()).ok_or_else(|| want("a uint64")),
        "d" => v.as_f64().map(|d| d.to_variant()).ok_or_else(|| want("a number")),
        "s" => v.as_str().map(|s| s.to_variant()).ok_or_else(|| want("a string")),
        "o" | "g" => {
            let s = v.as_str().ok_or_else(|| want("a string"))?;
            Variant::parse(Some(ty), &s.to_variant().print(false)).map_err(|e| format!("{s:?}: {}", e.message()))
        }
        "v" => {
            let t = v.get("type").and_then(Value::as_str).ok_or_else(|| want(r#"{"type": "<gvariant type>", "value": ...}"#))?;
            let inner_ty = VariantType::new(t).map_err(|e| format!("bad variant type '{t}': {}", e.message))?;
            let inner = json_to_variant(&inner_ty, v.get("value").unwrap_or(&Value::Null))?;
            Ok(Variant::from_variant(&inner))
        }
        _ if ty.is_maybe() => {
            let elem = ty.element();
            if v.is_null() {
                Ok(Variant::from_none(elem))
            } else {
                Ok(Variant::from_some(&json_to_variant(elem, v)?))
            }
        }
        _ if ty.is_array() => {
            let elem = ty.element();
            if elem.is_dict_entry() {
                let obj = v.as_object().ok_or_else(|| want("an object"))?;
                let key_ty = elem.key();
                if !is_string_like(key_ty) {
                    return Err(format!("dictionary keys of type '{}' are not JSON-expressible; use `value_text`", key_ty.as_str()));
                }
                let val_ty = elem.value();
                let mut items = Vec::with_capacity(obj.len());
                for (k, val) in obj {
                    let kv = json_to_variant(key_ty, &Value::String(k.clone()))?;
                    items.push(Variant::from_dict_entry(&kv, &json_to_variant(val_ty, val)?));
                }
                Ok(Variant::array_from_iter_with_type(elem, items))
            } else {
                let arr = v.as_array().ok_or_else(|| want("an array"))?;
                let items = arr.iter().map(|x| json_to_variant(elem, x)).collect::<Result<Vec<_>, _>>()?;
                Ok(Variant::array_from_iter_with_type(elem, items))
            }
        }
        _ if ty.is_tuple() => {
            let arr = v.as_array().ok_or_else(|| want("an array"))?;
            let mut items = Vec::new();
            let mut it = ty.first();
            while let Some(t) = it {
                let x = arr.get(items.len()).ok_or_else(|| want(&format!("{} elements", ty.n_items())))?;
                items.push(json_to_variant(t, x)?);
                it = t.next();
            }
            if items.len() != arr.len() {
                return Err(want(&format!("{} elements", items.len())));
            }
            Ok(Variant::tuple_from_iter(items))
        }
        _ => Err(format!("GVariant type '{}' is not JSON-expressible; use `value_text`", ty.as_str())),
    }
}

fn settings_path(s: &gio::Settings) -> Option<String> {
    s.path().map(|p| p.to_string())
}

fn key_json(settings: Option<&gio::Settings>, schema: &gio::SettingsSchema, name: &str) -> Value {
    let key = schema.key(name);
    let ty = key.value_type();
    let secret = key_is_secret(name);
    let mut m = Map::new();
    m.insert("name".into(), json!(name));
    m.insert("type".into(), json!(ty.as_str()));
    m.insert("summary".into(), json!(key.summary().map(|s| s.to_string())));
    m.insert("description".into(), json!(key.description().map(|s| s.to_string())));
    let red = || json!({ "redacted": true });
    m.insert("default".into(), if secret { red() } else { variant_to_json(&key.default_value()) });
    let range = key.range();
    if let Some(kind) = range.child_value(0).str().map(str::to_owned).filter(|k| k != "type") {
        m.insert("range".into(), json!({ "kind": kind, "data": variant_to_json(&range.child_value(1).as_variant().unwrap_or_else(|| range.child_value(1))) }));
    }
    if let Some(s) = settings {
        m.insert("value".into(), if secret { red() } else { variant_to_json(&s.value(name)) });
        m.insert("user_set".into(), json!(s.user_value(name).is_some()));
        m.insert("writable".into(), json!(s.is_writable(name)));
    }
    Value::Object(m)
}

/// A `GSettings` for `(schema, path)`: the app's registered object when there
/// is one, else a fresh instance (all instances share the backend).
fn settings_for(agent: &Agent, schema: &gio::SettingsSchema, path: Option<&str>) -> Result<Option<gio::Settings>, AgentError> {
    let id = schema.id();
    let want_path = path.map(str::to_owned).or_else(|| schema.path().map(|p| p.to_string()));
    if let Some(s) = agent.app_state.live_settings().into_iter().find(|s| {
        s.property::<Option<String>>("schema-id").as_deref() == Some(id.as_str())
            && (want_path.is_none() || want_path == settings_path(s))
    }) {
        return Ok(Some(s));
    }
    match (schema.path(), path) {
        (Some(_), _) => Ok(Some(gio::Settings::new_full(schema, gio::SettingsBackend::NONE, None))),
        (None, Some(p)) => {
            if !p.starts_with('/') || !p.ends_with('/') {
                return Err(AgentError::invalid_params("`path` must start and end with '/'"));
            }
            Ok(Some(gio::Settings::new_full(schema, gio::SettingsBackend::NONE, Some(p))))
        }
        (None, None) => Ok(None), // relocatable schema without a path: keys only
    }
}

fn schema_json(agent: &Agent, schema: &gio::SettingsSchema, path: Option<&str>, source_kind: &str) -> Result<Value, AgentError> {
    let settings = settings_for(agent, schema, path)?;
    let keys: Vec<Value> = schema.list_keys().iter().map(|k| key_json(settings.as_ref(), schema, k)).collect();
    Ok(json!({
        "id": schema.id().as_str(),
        "path": settings.as_ref().and_then(settings_path).or_else(|| schema.path().map(|p| p.to_string())),
        "relocatable": schema.path().is_none(),
        "source": source_kind,
        "keys": keys,
    }))
}

pub fn inspect_gsettings(agent: &Rc<Agent>, p: InspectGsettingsParams) -> Result<Value, AgentError> {
    let src = source()?;
    if let Some(id) = p.schema {
        let schema = src
            .lookup(&id, true)
            .ok_or_else(|| err(ErrorCode::NotFound, format!("schema '{id}' is not installed")))?;
        return Ok(json!({ "schemas": [schema_json(agent, &schema, p.path.as_deref(), "requested")?] }));
    }
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    // Settings objects the app registered come first: they are the ones in use.
    for s in agent.app_state.live_settings() {
        let Some(id) = s.property::<Option<String>>("schema-id") else { continue };
        let Some(schema) = src.lookup(&id, true) else { continue };
        let path = settings_path(&s).unwrap_or_default();
        if seen.contains(&(id.clone(), path.clone())) {
            continue;
        }
        out.push(schema_json(agent, &schema, Some(&path), "registered")?);
        seen.push((id, path));
    }
    if let Some(app_id) = agent.app.application_id() {
        let prefix = format!("{app_id}.");
        let (fixed, relocatable) = src.list_schemas(true);
        for id in fixed.iter().chain(relocatable.iter()) {
            let id = id.to_string();
            if id != app_id.as_str() && !id.starts_with(&prefix) {
                continue;
            }
            let Some(schema) = src.lookup(&id, true) else { continue };
            let path = schema.path().map(|p| p.to_string()).unwrap_or_default();
            if seen.iter().any(|(i, p)| *i == id && (p == &path || path.is_empty())) {
                continue;
            }
            out.push(schema_json(agent, &schema, None, "app_id_prefix")?);
            seen.push((id, path));
        }
    }
    Ok(json!({ "app_id": agent.app.application_id().map(|s| s.to_string()), "schemas": out }))
}

pub fn set_gsetting(agent: &Rc<Agent>, p: SetGsettingParams) -> Result<Value, AgentError> {
    if p.value.is_some() == p.value_text.is_some() {
        return Err(AgentError::invalid_params("pass exactly one of `value` and `value_text`"));
    }
    let src = source()?;
    let schema = src
        .lookup(&p.schema, true)
        .ok_or_else(|| err(ErrorCode::NotFound, format!("schema '{}' is not installed", p.schema)))?;
    if !schema.has_key(&p.key) {
        return Err(err(ErrorCode::NotFound, format!("schema '{}' has no key '{}'", p.schema, p.key))
            .with_data(json!({ "keys": schema.list_keys().iter().map(|k| k.to_string()).collect::<Vec<_>>() })));
    }
    let settings = settings_for(agent, &schema, p.path.as_deref())?.ok_or_else(|| {
        AgentError::invalid_params(format!("'{}' is a relocatable schema: pass `path`", p.schema))
    })?;
    if !settings.is_writable(&p.key) {
        return Err(err(ErrorCode::UnsupportedOperation, format!("key '{}' is not writable (locked down or read-only backend)", p.key)));
    }
    let key = schema.key(&p.key);
    let ty = key.value_type();
    let variant = match (&p.value, &p.value_text) {
        (Some(v), None) => json_to_variant(&ty, v).map_err(|m| err(ErrorCode::InvalidValue, m))?,
        (None, Some(t)) => Variant::parse(Some(&ty), t)
            .map_err(|e| err(ErrorCode::InvalidValue, format!("not valid GVariant text of type '{}': {}", ty.as_str(), e.message())))?,
        _ => unreachable!(),
    };
    if !key.range_check(&variant) {
        let range = key.range();
        return Err(err(
            ErrorCode::InvalidValue,
            format!("value {} is outside the range of key '{}'", variant.print(true), p.key),
        )
        .with_data(json!({ "range": variant_to_json(&range) })));
    }
    let secret = key_is_secret(&p.key);
    let previous = settings.value(&p.key);
    settings.set_value(&p.key, &variant).map_err(|e| err(ErrorCode::InvalidValue, format!("write rejected: {e}")))?;
    let observed = settings.value(&p.key);
    let red = || json!({ "redacted": true });
    Ok(json!({
        "schema": p.schema,
        "key": p.key,
        "type": ty.as_str(),
        "previous": if secret { red() } else { variant_to_json(&previous) },
        "value": if secret { red() } else { variant_to_json(&observed) },
        "changed": previous != observed,
    }))
}

// ---- resources ------------------------------------------------------------

const RES_FLAGS: gio::ResourceLookupFlags = gio::ResourceLookupFlags::NONE;
const MAX_RESOURCE_DEPTH: usize = 32;

fn res_path(p: Option<&str>) -> Result<String, AgentError> {
    let p = p.unwrap_or("/");
    if !p.starts_with('/') {
        return Err(AgentError::invalid_params(format!("resource path '{p}' must start with '/'")));
    }
    Ok(p.to_owned())
}

fn file_entry(path: &str) -> Option<Value> {
    gio::resources_get_info(path, RES_FLAGS)
        .ok()
        .map(|(size, flags)| json!({ "path": path, "kind": "file", "size": size, "compressed": flags & 1 != 0 }))
}

pub fn list_resources(p: ListResourcesParams) -> Result<Value, AgentError> {
    let root = res_path(p.path.as_deref())?;
    let limit = p.limit.unwrap_or(500).clamp(1, 10_000) as usize;
    let recursive = p.recursive.unwrap_or(false);
    if !root.ends_with('/') {
        return match file_entry(&root) {
            Some(e) => Ok(json!({ "path": root, "resources": [e], "truncated": false })),
            None => Err(err(ErrorCode::NotFound, format!("no resource at '{root}'"))),
        };
    }
    let mut out = Vec::new();
    let mut truncated = false;
    let mut stack = vec![(root.clone(), 0usize)];
    let mut first = true;
    while let Some((dir, depth)) = stack.pop() {
        let mut names: Vec<String> = match gio::resources_enumerate_children(&dir, RES_FLAGS) {
            Ok(n) => n.iter().map(|s| s.to_string()).collect(),
            Err(e) if first => return Err(err(ErrorCode::NotFound, format!("no resource directory '{dir}': {}", e.message()))),
            Err(_) => continue,
        };
        first = false;
        names.sort();
        let mut sub = Vec::new();
        for n in names {
            let full = format!("{dir}{n}");
            if n.ends_with('/') {
                out.push(json!({ "path": full, "kind": "directory" }));
                if recursive && depth < MAX_RESOURCE_DEPTH {
                    sub.push((full, depth + 1));
                }
            } else if let Some(e) = file_entry(&full) {
                out.push(e);
            }
            if out.len() >= limit {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
        stack.extend(sub.into_iter().rev());
    }
    Ok(json!({ "path": root, "resources": out, "truncated": truncated }))
}

pub fn read_resource(p: ReadResourceParams) -> Result<Value, AgentError> {
    let path = res_path(Some(&p.path))?;
    let max = p.max_bytes.unwrap_or(DEFAULT_RESOURCE_READ);
    if max == 0 || max > MAX_RESOURCE_READ {
        return Err(AgentError::invalid_params(format!("`max_bytes` must be 1..={MAX_RESOURCE_READ}")));
    }
    let bytes = gio::resources_lookup_data(&path, RES_FLAGS)
        .map_err(|e| err(ErrorCode::NotFound, format!("cannot read resource '{path}': {}", e.message())))?;
    let size = bytes.len();
    let take = size.min(max as usize);
    let mut data = &bytes[..take];
    let truncated = take < size;
    let content_type = gio::content_type_guess(Some(&path), data).0.to_string();
    let text = match p.encoding.unwrap_or_default() {
        ResourceEncoding::Base64 => None,
        enc => {
            let mut end = data.len();
            // A cut inside a multi-byte character is not "binary": drop the partial tail.
            let valid = loop {
                match std::str::from_utf8(&data[..end]) {
                    Ok(s) => break Some(s),
                    Err(e) if truncated && e.error_len().is_none() => end = e.valid_up_to(),
                    Err(_) => break None,
                }
            };
            match (valid, enc) {
                (Some(s), _) => {
                    data = &data[..s.len()];
                    Some(s.to_owned())
                }
                (None, ResourceEncoding::Text) => {
                    return Err(err(ErrorCode::InvalidValue, format!("resource '{path}' is not valid UTF-8 text; use encoding base64")));
                }
                (None, _) => None,
            }
        }
    };
    let (encoding, content) = match text {
        Some(t) => ("utf8", t),
        None => ("base64", STANDARD.encode(data)),
    };
    Ok(json!({
        "path": path,
        "size": size,
        "returned": data.len(),
        "truncated": truncated,
        "content_type": content_type,
        "encoding": encoding,
        "content": content,
    }))
}

// ---- object_stats ----------------------------------------------------------

pub fn object_stats(agent: &Rc<Agent>, p: ObjectStatsParams) -> Result<Value, AgentError> {
    if !agent.config.instance_count {
        return Err(err(
            ErrorCode::Unavailable,
            "instance counts are off: the app must start with GOBJECT_DEBUG=instance-count (launch_app preset `instance_count`)",
        ));
    }
    // GOBJECT_DEBUG was set, but GLib only counts when it was built with debugging
    // support: probe with a live object instead of trusting the environment.
    let probe = glib::Object::new::<glib::Object>();
    // SAFETY: plain query of a registered GType.
    let probed = unsafe { glib::gobject_ffi::g_type_get_instance_count(glib::Object::static_type().into_glib()) };
    drop(probe);
    if probed < 1 {
        return Err(err(
            ErrorCode::Unavailable,
            "GOBJECT_DEBUG=instance-count is set but this GLib does not count instances (built without debug support)",
        ));
    }
    let min = p.min_count.unwrap_or(1).max(1) as i64;
    let limit = p.limit.unwrap_or(200).clamp(1, 5000) as usize;
    let mut types = Vec::new();
    let mut stack = vec![glib::Object::static_type()];
    let mut scanned = 0usize;
    while let Some(t) = stack.pop() {
        stack.extend(t.children());
        scanned += 1;
        // SAFETY: plain query of a registered GType.
        let n = i64::from(unsafe { glib::gobject_ffi::g_type_get_instance_count(t.into_glib()) });
        if n >= min && p.type_prefix.as_deref().is_none_or(|pre| t.name().starts_with(pre)) {
            types.push((t.name().to_string(), t.parent().map(|p| p.name().to_string()), n));
        }
    }
    types.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let total: i64 = types.iter().map(|t| t.2).sum();
    let matched = types.len();
    types.truncate(limit);
    Ok(json!({
        "types": types.iter().map(|(name, parent, n)| json!({"type": name, "parent": parent, "count": n})).collect::<Vec<_>>(),
        "matched_types": matched,
        "types_scanned": scanned,
        "total_instances": total,
        "truncated": matched > limit,
    }))
}
