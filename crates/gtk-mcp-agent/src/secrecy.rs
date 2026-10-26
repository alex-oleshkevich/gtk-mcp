//! Secrecy model (plan 4.4). Secrecy is a property of the object, computed
//! centrally and enforced in the value serializer. It is sticky for the
//! object's lifetime (peeking does not clear it).

use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use serde_json::{Value, json};

const FLAG_SECRET: u8 = 1;
const FLAG_NOT_SECRET: u8 = 2;

fn secret_quark() -> glib::Quark {
    glib::Quark::from_str("gtk-mcp-secret")
}

fn flag(obj: &glib::Object) -> Option<u8> {
    // SAFETY: the quark is only ever used with `u8` values.
    unsafe { obj.qdata::<u8>(secret_quark()).map(|p| *p.as_ref()) }
}

fn set_flag(obj: &glib::Object, v: u8) {
    unsafe { obj.set_qdata(secret_quark(), v) };
}

pub fn is_type_name(obj: &impl IsA<glib::Object>, name: &str) -> bool {
    glib::Type::from_name(name).is_some_and(|t| obj.type_().is_a(t))
}

/// `{"redacted": true}`, the serialized form of any withheld value.
pub fn redacted() -> Value {
    json!({"redacted": true})
}

/// `mark_secret` public API: the widget joins the secret set (descendants,
/// delegate and buffers follow from [`is_secret`]).
pub fn mark_secret(obj: &impl IsA<glib::Object>) {
    let obj = obj.upcast_ref::<glib::Object>();
    set_flag(obj, FLAG_SECRET);
    propagate(obj);
}

/// Is this a secret *root* judged on its own properties?
fn is_root_by_properties(obj: &glib::Object) -> bool {
    if obj.is::<gtk::PasswordEntry>() || is_type_name(obj, "AdwPasswordEntryRow") {
        return true;
    }
    let editable_like = obj.is::<gtk::Entry>() || obj.is::<gtk::Text>();
    if !editable_like {
        return false;
    }
    let purpose: gtk::InputPurpose = obj.property("input-purpose");
    if matches!(purpose, gtk::InputPurpose::Password | gtk::InputPurpose::Pin) {
        return true;
    }
    // `visibility=false` counts only when first observed (before a peek toggles it).
    if flag(obj).is_none() {
        let visible: bool = obj.property("visibility");
        if !visible {
            return true;
        }
    }
    false
}

/// A secret root's delegate and buffer join the secret set.
fn propagate(obj: &glib::Object) {
    if let Some(editable) = obj.dynamic_cast_ref::<gtk::Editable>() {
        if let Some(delegate) = editable.delegate() {
            let d = delegate.upcast::<glib::Object>();
            if &d != obj && flag(&d) != Some(FLAG_SECRET) {
                set_flag(&d, FLAG_SECRET);
                propagate(&d);
            }
        }
    }
    let buffer: Option<gtk::EntryBuffer> = if let Some(e) = obj.dynamic_cast_ref::<gtk::Entry>() {
        Some(e.buffer())
    } else {
        obj.dynamic_cast_ref::<gtk::Text>().map(|t| t.buffer())
    };
    if let Some(b) = buffer {
        set_flag(b.upcast_ref(), FLAG_SECRET);
    }
}

/// Is `obj` in the secret set: a root, a descendant of a root, a root's
/// editable delegate, or a root's buffer.
pub fn is_secret(obj: &impl IsA<glib::Object>) -> bool {
    let obj = obj.upcast_ref::<glib::Object>();
    if flag(obj) == Some(FLAG_SECRET) {
        return true;
    }
    if let Some(w) = obj.dynamic_cast_ref::<gtk::Widget>() {
        let mut cur = Some(w.clone());
        while let Some(c) = cur {
            let co = c.upcast_ref::<glib::Object>();
            if flag(co) == Some(FLAG_SECRET) || is_root_by_properties(co) {
                set_flag(co, FLAG_SECRET);
                propagate(co);
                if co != obj {
                    set_flag(obj, FLAG_SECRET);
                    propagate(obj);
                }
                return true;
            }
            if (c.is::<gtk::Entry>() || c.is::<gtk::Text>()) && flag(co).is_none() {
                set_flag(co, FLAG_NOT_SECRET); // first observation recorded
            }
            cur = c.parent();
        }
        return false;
    }
    false
}

/// Called when a non-widget object is discovered through `owner`
/// (e.g. the `buffer` property): inherits the owner's secrecy.
pub fn inherit(owner: &glib::Object, child: &glib::Object) {
    if is_secret(owner) {
        set_flag(child, FLAG_SECRET);
    }
}

const SAFE_STRING_PROPS: &[&str] = &["name", "css-name", "placeholder-text", "tooltip-text", "tooltip-markup", "icon-name"];
const SECRET_NUMERIC_PROPS: &[&str] =
    &["text-length", "length", "cursor-position", "selection-bound", "scroll-offset", "max-length", "preedit-length"];

/// Must the value of `pspec` on secret object `obj` be withheld?
pub fn redact_property(pspec: &glib::ParamSpec) -> bool {
    let name = pspec.name();
    let vt = pspec.value_type();
    if vt == glib::Type::STRING || vt == <Vec<String>>::static_type() {
        return !SAFE_STRING_PROPS.contains(&name);
    }
    SECRET_NUMERIC_PROPS.contains(&name)
}

/// Does the subtree rooted at `w` (inclusive) contain a secret object that is
/// currently revealing its content (`visibility=true`)? Used by `capture` and
/// `inspect_render` (plan 4.4).
pub fn subtree_peeking(w: &gtk::Widget) -> bool {
    fn walk(w: &gtk::Widget) -> bool {
        if (w.is::<gtk::Entry>() || w.is::<gtk::Text>()) && is_secret(w) {
            let visible: bool = w.property("visibility");
            if visible {
                return true;
            }
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if walk(&child) {
                return true;
            }
            c = child.next_sibling();
        }
        false
    }
    walk(w)
}
