//! JSON <-> `GValue` conversion for `get_property` / `set_property`,
//! `inspect_model` summaries and `wait_for` property conditions (tools.md,
//! plan 4.4). Reading is secret-aware: the owner's secrecy is decided here, in
//! the one serializer. Error messages never echo the offered value (it may be
//! a secret being written), only its JSON kind.

use crate::agent::Agent;
use crate::secrecy;
use glib::prelude::*;
use glib::translate::*;
use gtk4::gdk;
use gtk_mcp_protocol::{AgentError, ErrorCode};
use serde_json::{Number, Value, json};

fn f64_json(x: f64) -> Value {
    Number::from_f64(x).map(Value::Number).unwrap_or_else(|| Value::String(x.to_string()))
}

fn f32_json(x: f32) -> Value {
    if x.is_finite() { f64_json(x.to_string().parse().unwrap_or(x as f64)) } else { Value::String(x.to_string()) }
}

/// The JSON kind of `v` for error messages, which never echo offered values.
pub fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `{"unsupported_type": "<GType>"}`.
pub fn unsupported_type(t: glib::Type) -> Value {
    json!({"unsupported_type": t.name()})
}

/// `{"ref", "type"}` of an object, inheriting the owner's secrecy first for
/// non-widgets (plan 4.4).
pub fn object_json(agent: &Agent, owner: Option<&glib::Object>, obj: &glib::Object) -> Value {
    if let Some(owner) = owner {
        if !obj.is::<gtk4::Widget>() {
            secrecy::inherit(owner, obj);
        }
    }
    json!({"ref": agent.registry.ref_for(obj), "type": obj.type_().name()})
}

/// The object held by an object-typed value (also interface-typed values).
pub fn object_of(value: &glib::Value) -> Option<glib::Object> {
    if !value.type_().is_a(glib::Type::OBJECT) {
        return None;
    }
    // SAFETY: the value holds an object type; the getter returns a borrowed pointer.
    unsafe {
        let ptr = glib::gobject_ffi::g_value_get_object(value.to_glib_none().0);
        if ptr.is_null() { None } else { Some(from_glib_none(ptr)) }
    }
}

/// Owner-independent JSON of scalar-like values; `None` for objects and
/// unsupported types. Strings, numbers, booleans, enums, flags, strv, GVariant
/// and GdkRGBA are covered.
pub fn scalar_to_json(pspec: &glib::ParamSpec, value: &glib::Value) -> Option<Value> {
    let t = value.type_();
    if pspec.downcast_ref::<glib::ParamSpecUnichar>().is_some() {
        return Some(match value.get::<u32>().ok().and_then(char::from_u32) {
            Some(c) => Value::String(c.to_string()),
            None => Value::Null,
        });
    }
    if t == glib::Type::BOOL {
        value.get::<bool>().ok().map(Value::Bool)
    } else if t == glib::Type::I8 {
        value.get::<i8>().ok().map(|v| json!(v))
    } else if t == glib::Type::U8 {
        value.get::<u8>().ok().map(|v| json!(v))
    } else if t == glib::Type::I32 {
        value.get::<i32>().ok().map(|v| json!(v))
    } else if t == glib::Type::U32 {
        value.get::<u32>().ok().map(|v| json!(v))
    } else if t == glib::Type::I64 {
        value.get::<i64>().ok().map(|v| json!(v))
    } else if t == glib::Type::U64 {
        value.get::<u64>().ok().map(|v| json!(v))
    } else if t == glib::Type::I_LONG {
        // SAFETY: the value holds a `long`.
        Some(json!(unsafe { glib::gobject_ffi::g_value_get_long(value.to_glib_none().0) } as i64))
    } else if t == glib::Type::U_LONG {
        // SAFETY: the value holds an `unsigned long`.
        Some(json!(unsafe { glib::gobject_ffi::g_value_get_ulong(value.to_glib_none().0) } as u64))
    } else if t == glib::Type::F32 {
        value.get::<f32>().ok().map(f32_json)
    } else if t == glib::Type::F64 {
        value.get::<f64>().ok().map(f64_json)
    } else if t == glib::Type::STRING {
        value.get::<Option<String>>().ok().map(|s| s.map_or(Value::Null, Value::String))
    } else if t.is_a(glib::Type::ENUM) {
        match glib::EnumValue::from_value(value) {
            Some((_, ev)) => Some(json!(ev.nick())),
            // SAFETY: the value holds an enum.
            None => Some(json!(unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) })),
        }
    } else if t.is_a(glib::Type::FLAGS) {
        Some(match glib::FlagsValue::from_value(value) {
            Some((_, vs)) => Value::Array(vs.iter().map(|f| json!(f.nick())).collect()),
            None => json!([]),
        })
    } else if t == <Vec<String>>::static_type() {
        Some(Value::Array(value.get::<Vec<String>>().unwrap_or_default().into_iter().map(Value::String).collect()))
    } else if t == glib::Type::VARIANT {
        value.get::<Option<glib::Variant>>().ok().map(|v| v.map_or(Value::Null, |v| Value::String(v.print(true).to_string())))
    } else if t == gdk::RGBA::static_type() {
        value.get::<Option<gdk::RGBA>>().ok().map(|c| c.map_or(Value::Null, |c| Value::String(c.to_str().to_string())))
    } else {
        None
    }
}

/// JSON of `value` (the value of `pspec` on `owner`). Withheld values are
/// `{"redacted": true}`; objects are refs; unsupported types are
/// `{"unsupported_type": "<GType>"}`. Never panics.
pub fn value_to_json(agent: &Agent, owner: &glib::Object, pspec: &glib::ParamSpec, value: &glib::Value) -> Value {
    if secrecy::is_secret(owner) && secrecy::redact_property(pspec) {
        return secrecy::redacted();
    }
    if let Some(v) = scalar_to_json(pspec, value) {
        return v;
    }
    if value.type_().is_a(glib::Type::OBJECT) {
        return match object_of(value) {
            Some(o) => object_json(agent, Some(owner), &o),
            None => Value::Null,
        };
    }
    unsupported_type(value.type_())
}

/// Minimum / maximum of a numeric pspec as JSON.
pub fn numeric_range(pspec: &glib::ParamSpec) -> Option<(Value, Value)> {
    macro_rules! r {
        ($t:ty) => {
            if let Some(p) = pspec.downcast_ref::<$t>() {
                return Some((json!(p.minimum()), json!(p.maximum())));
            }
        };
    }
    r!(glib::ParamSpecChar);
    r!(glib::ParamSpecUChar);
    r!(glib::ParamSpecInt);
    r!(glib::ParamSpecUInt);
    r!(glib::ParamSpecLong);
    r!(glib::ParamSpecULong);
    r!(glib::ParamSpecInt64);
    r!(glib::ParamSpecUInt64);
    if let Some(p) = pspec.downcast_ref::<glib::ParamSpecFloat>() {
        return Some((f32_json(p.minimum()), f32_json(p.maximum())));
    }
    if let Some(p) = pspec.downcast_ref::<glib::ParamSpecDouble>() {
        return Some((f64_json(p.minimum()), f64_json(p.maximum())));
    }
    None
}

fn int_range(pspec: &glib::ParamSpec) -> Option<(i128, i128)> {
    macro_rules! r {
        ($t:ty) => {
            if let Some(p) = pspec.downcast_ref::<$t>() {
                return Some((p.minimum() as i128, p.maximum() as i128));
            }
        };
    }
    r!(glib::ParamSpecChar);
    r!(glib::ParamSpecUChar);
    r!(glib::ParamSpecInt);
    r!(glib::ParamSpecUInt);
    r!(glib::ParamSpecLong);
    r!(glib::ParamSpecULong);
    r!(glib::ParamSpecInt64);
    r!(glib::ParamSpecUInt64);
    None
}

fn as_i128(v: &Value) -> Option<i128> {
    let Value::Number(n) = v else { return None };
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
        .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e18).map(|f| f as i128))
}

fn enum_nick_matches(nick: &str, name: &str, wanted: &str) -> bool {
    let norm = |s: &str| s.replace('_', "-").to_ascii_lowercase();
    let w = norm(wanted);
    norm(nick) == w || norm(name) == w
}

/// Convert `v` to a `GValue` for `pspec`, rejecting read-only and
/// construct-only properties. A secret object may not be assigned as a
/// property value: re-homing e.g. a password entry's buffer onto a plain
/// entry would expose the secret through the new owner (gm-lhp). The
/// refusal is `Redacted`, and the message never echoes the offered value.
pub fn json_to_value(agent: &Agent, pspec: &glib::ParamSpec, v: &Value) -> Result<glib::Value, AgentError> {
    let flags = pspec.flags();
    if !flags.contains(glib::ParamFlags::WRITABLE) {
        return Err(AgentError::new(
            ErrorCode::UnsupportedOperation,
            format!("property '{}' of {} is read-only", pspec.name(), pspec.owner_type().name()),
        ));
    }
    if flags.contains(glib::ParamFlags::CONSTRUCT_ONLY) {
        return Err(AgentError::new(
            ErrorCode::UnsupportedOperation,
            format!("property '{}' of {} can only be set at construction", pspec.name(), pspec.owner_type().name()),
        ));
    }
    let val = json_to_value_unchecked(agent, pspec, v)?;
    if let Some(obj) = object_of(&val) {
        if secrecy::is_secret(&obj) {
            return Err(AgentError::new(
                ErrorCode::Redacted,
                format!("property '{}' of {} cannot be assigned a secret object", pspec.name(), pspec.owner_type().name()),
            ));
        }
    }
    Ok(val)
}

/// Conversion without the writability checks (also used to normalize the
/// `equals` operand of `wait_for` property conditions).
pub fn json_to_value_unchecked(agent: &Agent, pspec: &glib::ParamSpec, v: &Value) -> Result<glib::Value, AgentError> {
    let vt = pspec.value_type();
    let name = pspec.name();
    let bad = |msg: String| AgentError::new(ErrorCode::InvalidValue, format!("property '{name}' ({}): {msg}", vt.name()));
    let expected = |what: &str| bad(format!("expected {what}, got {}", kind(v)));

    if pspec.downcast_ref::<glib::ParamSpecUnichar>().is_some() {
        let s = v.as_str().ok_or_else(|| expected("a one-character string"))?;
        let mut it = s.chars();
        return match (it.next(), it.next()) {
            (Some(c), None) => Ok((c as u32).to_value()),
            _ => Err(bad("expected exactly one character".into())),
        };
    }
    if vt == glib::Type::BOOL {
        return v.as_bool().map(|b| b.to_value()).ok_or_else(|| expected("a boolean"));
    }
    if let Some((min, max)) = int_range(pspec) {
        let n = match v {
            // A single ASCII character stands for its code (char / uchar).
            Value::String(s) if (vt == glib::Type::I8 || vt == glib::Type::U8) && s.len() == 1 => i128::from(s.as_bytes()[0]),
            _ => as_i128(v).ok_or_else(|| expected("an integer"))?,
        };
        if n < min || n > max {
            return Err(bad(format!("integer out of range {min}..={max}")));
        }
        return Ok(if vt == glib::Type::I8 {
            (n as i8).to_value()
        } else if vt == glib::Type::U8 {
            (n as u8).to_value()
        } else if vt == glib::Type::I32 {
            (n as i32).to_value()
        } else if vt == glib::Type::U32 {
            (n as u32).to_value()
        } else if vt == glib::Type::I64 {
            (n as i64).to_value()
        } else if vt == glib::Type::U64 {
            (n as u64).to_value()
        } else if vt == glib::Type::I_LONG {
            let mut val = glib::Value::from_type(glib::Type::I_LONG);
            // SAFETY: `val` holds a `long`.
            unsafe { glib::gobject_ffi::g_value_set_long(val.to_glib_none_mut().0, n as std::ffi::c_long) };
            val
        } else {
            let mut val = glib::Value::from_type(glib::Type::U_LONG);
            // SAFETY: `val` holds an `unsigned long`.
            unsafe { glib::gobject_ffi::g_value_set_ulong(val.to_glib_none_mut().0, n as std::ffi::c_ulong) };
            val
        });
    }
    if vt == glib::Type::F32 || vt == glib::Type::F64 {
        let f = v.as_f64().ok_or_else(|| expected("a number"))?;
        let (min, max) = numeric_range(pspec)
            .and_then(|(a, b)| Some((a.as_f64()?, b.as_f64()?)))
            .unwrap_or((f64::MIN, f64::MAX));
        if !(min..=max).contains(&f) {
            return Err(bad(format!("number out of range {min}..={max}")));
        }
        return Ok(if vt == glib::Type::F32 { (f as f32).to_value() } else { f.to_value() });
    }
    if vt == glib::Type::STRING {
        return match v {
            Value::Null => Ok(None::<&str>.to_value()),
            Value::String(s) => Ok(s.to_value()),
            _ => Err(expected("a string or null")),
        };
    }
    if vt.is_a(glib::Type::ENUM) {
        let class = glib::EnumClass::with_type(vt).ok_or_else(|| bad("enum class unavailable".into()))?;
        let valid = || class.values().iter().map(|e| e.nick()).collect::<Vec<_>>().join(", ");
        let ev = match v {
            Value::String(s) => class
                .values()
                .iter()
                .find(|e| enum_nick_matches(e.nick(), e.name(), s))
                .ok_or_else(|| bad(format!("unknown enum nick; valid: {}", valid())))?
                .value(),
            Value::Number(_) => {
                let n = as_i128(v).ok_or_else(|| expected("an enum nick or integer"))?;
                let n = i32::try_from(n).map_err(|_| bad("integer out of range for an enum".into()))?;
                class.value(n).ok_or_else(|| bad(format!("not a member; valid: {}", valid())))?.value()
            }
            _ => return Err(expected("an enum nick or integer")),
        };
        return class.to_value(ev).ok_or_else(|| bad("enum value rejected".into()));
    }
    if vt.is_a(glib::Type::FLAGS) {
        let class = glib::FlagsClass::with_type(vt).ok_or_else(|| bad("flags class unavailable".into()))?;
        let valid = || class.values().iter().map(|e| e.nick()).collect::<Vec<_>>().join(", ");
        let mask = class.values().iter().fold(0u32, |m, f| m | f.value());
        let bits = match v {
            Value::Array(items) => {
                let mut bits = 0u32;
                for item in items {
                    bits |= match item {
                        Value::String(s) => class
                            .values()
                            .iter()
                            .find(|e| enum_nick_matches(e.nick(), e.name(), s))
                            .ok_or_else(|| bad(format!("unknown flag nick; valid: {}", valid())))?
                            .value(),
                        _ => {
                            let n = as_i128(item).ok_or_else(|| expected("an array of flag nicks or integers"))?;
                            u32::try_from(n).map_err(|_| bad("flag integer out of range".into()))?
                        }
                    };
                }
                bits
            }
            Value::Number(_) => {
                let n = as_i128(v).ok_or_else(|| expected("an array of flag nicks or an integer"))?;
                u32::try_from(n).map_err(|_| bad("flags integer out of range".into()))?
            }
            _ => return Err(expected("an array of flag nicks or an integer")),
        };
        if bits & !mask != 0 {
            return Err(bad(format!("unknown flag bits; valid: {}", valid())));
        }
        let mut val = glib::Value::from_type(vt);
        // SAFETY: `val` holds a flags type and `bits` only contains its members.
        unsafe { glib::gobject_ffi::g_value_set_flags(val.to_glib_none_mut().0, bits) };
        return Ok(val);
    }
    if vt == <Vec<String>>::static_type() {
        let Value::Array(items) = v else { return Err(expected("an array of strings")) };
        let strs: Option<Vec<String>> = items.iter().map(|i| i.as_str().map(String::from)).collect();
        return strs.map(|s| s.to_value()).ok_or_else(|| bad("every array element must be a string".into()));
    }
    if vt == glib::Type::VARIANT {
        let ty = pspec
            .downcast_ref::<glib::ParamSpecVariant>()
            .and_then(|p| p.type_().filter(|t| t.is_definite()).map(|t| t.to_owned()));
        return match v {
            Value::Null => Ok(None::<glib::Variant>.to_value()),
            Value::String(s) => glib::Variant::parse(ty.as_deref(), s)
                .map(|var| Some(var).to_value())
                .map_err(|e| bad(format!("not a valid GVariant text: {}", e.message()))),
            _ => Err(expected("GVariant text (a string) or null")),
        };
    }
    if vt == gdk::RGBA::static_type() {
        return match v {
            Value::Null => Ok(None::<gdk::RGBA>.to_value()),
            Value::String(s) => {
                gdk::RGBA::parse(s.as_str()).map(|c| Some(c).to_value()).map_err(|_| bad("not a CSS color".into()))
            }
            _ => Err(expected("a CSS color string or null")),
        };
    }
    if vt.is_a(glib::Type::OBJECT) {
        let r = match v {
            Value::Null => None,
            Value::String(s) => Some(s.as_str()),
            Value::Object(m) => Some(m.get("ref").and_then(Value::as_str).ok_or_else(|| expected("a reference string, {\"ref\": ...} or null"))?),
            _ => return Err(expected("a reference string, {\"ref\": ...} or null")),
        };
        let mut val = glib::Value::from_type(vt);
        if let Some(r) = r {
            let obj = agent.registry.resolve(r)?;
            if !obj.type_().is_a(vt) {
                return Err(bad(format!("'{r}' is a {}, which is not a {}", obj.type_().name(), vt.name())));
            }
            // SAFETY: `val` holds an object type that `obj` is an instance of.
            unsafe { glib::gobject_ffi::g_value_set_object(val.to_glib_none_mut().0, obj.as_ptr() as *mut _) };
        }
        return Ok(val);
    }
    Err(AgentError::new(
        ErrorCode::UnsupportedOperation,
        format!("properties of type {} cannot be set through JSON", vt.name()),
    ))
}
