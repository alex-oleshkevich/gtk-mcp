//! `inspect_settings` and `override_setting` (tools.md layout-style).
//!
//! GtkSettings is read through its GObject properties. libadwaita is never
//! linked (plan 6): `AdwStyleManager` is found by GType name and
//! `adw_style_manager_get_default` through `dlsym`, then read and written
//! through its properties.
//!
//! Overrides are session-scoped. The value a setting had before the first
//! override is remembered; it is restored when the last client that overrode it
//! disconnects ([`release_conn`]), on `reset`, and at shutdown ([`release_all`]).

use crate::agent::{Agent, Ctx};
use crate::json::{json_to_value, scalar_to_json};
use glib::prelude::*;
use glib::translate::from_glib_none;
use gtk4 as gtk;
use gtk_mcp_protocol::{AgentError, ErrorCode, InspectSettingsParams, OverrideSettingParams};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const ADW_PROPERTIES: &[&str] = &[
    "color-scheme",
    "dark",
    "high-contrast",
    "system-supports-color-schemes",
    "accent-color",
    "accent-color-rgba",
    "system-supports-accent-colors",
];
const DEFAULT_DPI: i32 = 96 * 1024;
const MIN_TEXT_SCALE: f64 = 0.25;
const MAX_TEXT_SCALE: f64 = 4.0;

/// The libadwaita style manager, when libadwaita is loaded and initialized.
fn style_manager() -> Option<glib::Object> {
    glib::Type::from_name("AdwStyleManager")?;
    let sym = |name: &std::ffi::CStr| {
        // SAFETY: plain lookup in the global namespace.
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
    };
    let init = sym(c"adw_is_initialized");
    if !init.is_null() {
        // SAFETY: `gboolean adw_is_initialized(void)`.
        let f: unsafe extern "C" fn() -> glib::ffi::gboolean = unsafe { std::mem::transmute(init) };
        if unsafe { f() } == 0 {
            return None;
        }
    }
    let get = sym(c"adw_style_manager_get_default");
    if get.is_null() {
        return None;
    }
    // SAFETY: `AdwStyleManager *adw_style_manager_get_default(void)`, transfer none.
    let f: unsafe extern "C" fn() -> *mut glib::gobject_ffi::GObject = unsafe { std::mem::transmute(get) };
    let ptr = unsafe { f() };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null GObject, borrowed from the singleton; `from_glib_none` takes a ref.
    Some(unsafe { from_glib_none(ptr) })
}

fn read_props(obj: &glib::Object, names: &[&str]) -> Map<String, Value> {
    let mut m = Map::new();
    for name in names {
        let Some(ps) = obj.find_property(name) else { continue };
        if !ps.flags().contains(glib::ParamFlags::READABLE) {
            continue;
        }
        if let Some(v) = scalar_to_json(&ps, &obj.property_value(name)) {
            m.insert((*name).to_string(), v);
        }
    }
    m
}

fn direction_name(d: gtk::TextDirection) -> &'static str {
    match d {
        gtk::TextDirection::Ltr => "ltr",
        gtk::TextDirection::Rtl => "rtl",
        _ => "none",
    }
}

fn settings() -> Result<gtk::Settings, AgentError> {
    gtk::Settings::default().ok_or_else(|| AgentError::new(ErrorCode::Unavailable, "there is no default GtkSettings (no display)"))
}

/// `gtk-application-prefer-dark-theme` through its GObject property. The
/// typed accessors are deprecated since GTK 4.20 and have no replacement
/// API (without libadwaita, handled above, this GtkSettings property is
/// the only way to request a dark theme); the generic property accessors
/// are not deprecated and the property exists well below the 4.10 floor.
fn prefer_dark(s: &gtk::Settings) -> bool {
    s.property::<bool>("gtk-application-prefer-dark-theme")
}

fn set_prefer_dark(s: &gtk::Settings, dark: bool) {
    s.set_property("gtk-application-prefer-dark-theme", dark);
}

pub fn inspect_settings(_agent: &Rc<Agent>, _p: InspectSettingsParams) -> Result<Value, AgentError> {
    let settings = settings()?;
    let mut gtk_props = Map::new();
    for ps in settings.list_properties() {
        if !ps.flags().contains(glib::ParamFlags::READABLE) {
            continue;
        }
        if let Some(v) = scalar_to_json(&ps, &settings.property_value(ps.name())) {
            gtk_props.insert(ps.name().to_string(), v);
        }
    }
    let adw = match style_manager() {
        Some(sm) => {
            let mut m = read_props(&sm, ADW_PROPERTIES);
            m.insert("read_only".into(), json!(["high-contrast", "accent-color", "accent-color-rgba", "system-supports-accent-colors"]));
            json!({"available": true, "style_manager": m})
        }
        None => json!({"available": false}),
    };
    let overrides: Vec<Value> = OVERRIDES.with(|o| {
        o.borrow().iter().map(|(k, rec)| json!({"setting": k, "clients": rec.owners.len()})).collect()
    });
    Ok(json!({
        "gtk": gtk_props,
        "default_text_direction": direction_name(gtk::Widget::default_direction()),
        "libadwaita": adw,
        "overrides": overrides,
    }))
}

// -- overrides --------------------------------------------------------------------

enum Prev {
    ColorScheme(glib::Value),
    PreferDark(bool),
    Animations(bool),
    XftDpi(i32),
    Direction(gtk::TextDirection),
}

struct Rec {
    previous: Prev,
    owners: Vec<u64>,
}

thread_local! {
    static OVERRIDES: RefCell<BTreeMap<&'static str, Rec>> = const { RefCell::new(BTreeMap::new()) };
}

fn restore(prev: Prev) {
    match prev {
        Prev::ColorScheme(v) => {
            if let Some(sm) = style_manager() {
                sm.set_property_from_value("color-scheme", &v);
            }
        }
        Prev::PreferDark(b) => {
            if let Some(s) = gtk::Settings::default() {
                set_prefer_dark(&s, b);
            }
        }
        Prev::Animations(b) => {
            if let Some(s) = gtk::Settings::default() {
                s.set_gtk_enable_animations(b);
            }
        }
        Prev::XftDpi(d) => {
            if let Some(s) = gtk::Settings::default() {
                s.set_gtk_xft_dpi(d);
            }
        }
        Prev::Direction(d) => gtk::Widget::set_default_direction(d),
    }
}

/// A connection dropped: overrides only it held are reverted.
pub fn release_conn(conn: u64) {
    let gone: Vec<Prev> = OVERRIDES.with(|o| {
        let mut o = o.borrow_mut();
        for rec in o.values_mut() {
            rec.owners.retain(|c| *c != conn);
        }
        let keys: Vec<&'static str> = o.iter().filter(|(_, r)| r.owners.is_empty()).map(|(k, _)| *k).collect();
        keys.into_iter().filter_map(|k| o.remove(k)).map(|r| r.previous).collect()
    });
    for p in gone {
        restore(p);
    }
}

pub fn release_all() {
    let all = OVERRIDES.with(|o| std::mem::take(&mut *o.borrow_mut()));
    for (_, rec) in all {
        restore(rec.previous);
    }
}

fn bad(msg: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::InvalidValue, msg)
}

/// Current state and the JSON of `previous` (the value before the first override).
struct Applied {
    previous: Value,
    observed: Value,
}

fn color_scheme_json(sm: &glib::Object) -> Value {
    let mut m = read_props(sm, &["color-scheme", "dark"]);
    m.remove("accent-color");
    Value::Object(m)
}

pub fn override_setting(_agent: &Rc<Agent>, p: OverrideSettingParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let key: &'static str = match p.setting.replace('-', "_").as_str() {
        "color_scheme" => "color_scheme",
        "animations" => "animations",
        "text_scale" => "text_scale",
        "text_direction" => "text_direction",
        "high_contrast" | "accent_color" | "accent" => {
            return Err(AgentError::new(
                ErrorCode::UnsupportedOperation,
                format!("'{}' is read-only (libadwaita derives it from the system); it can be inspected, not overridden", p.setting),
            ));
        }
        other => {
            return Err(AgentError::invalid_params(format!(
                "unknown setting '{other}'; valid: color_scheme, animations, text_scale, text_direction"
            )));
        }
    };
    let conn = ctx.conn.id;

    if p.reset.unwrap_or(false) {
        let prev = OVERRIDES.with(|o| {
            let mut o = o.borrow_mut();
            let rec = o.get_mut(key).filter(|r| r.owners.contains(&conn))?;
            rec.owners.retain(|c| *c != conn);
            if rec.owners.is_empty() { o.remove(key).map(|r| Some(r.previous)) } else { Some(None) }
        });
        return match prev {
            None => Err(AgentError::new(ErrorCode::NotFound, format!("this client has no active override of '{key}'"))),
            Some(Some(p)) => {
                restore(p);
                Ok(json!({"setting": key, "reset": true, "restored": true}))
            }
            Some(None) => Ok(json!({"setting": key, "reset": true, "restored": false, "reason": "another client's override stays active"})),
        };
    }
    let value = p.value.as_ref().ok_or_else(|| AgentError::invalid_params("value is required unless reset is true"))?;

    let settings = settings()?;
    let (prev, applied) = match key {
        "color_scheme" => match style_manager() {
            Some(sm) => {
                let ps = sm.find_property("color-scheme").ok_or_else(|| {
                    AgentError::new(ErrorCode::UnsupportedOperation, "this libadwaita has no AdwStyleManager:color-scheme")
                })?;
                let new = json_to_value(_agent, &ps, value)?;
                let previous = sm.property_value("color-scheme");
                let previous_json = scalar_to_json(&ps, &previous).unwrap_or(Value::Null);
                sm.set_property_from_value("color-scheme", &new);
                (Prev::ColorScheme(previous), Applied { previous: previous_json, observed: color_scheme_json(&sm) })
            }
            None => {
                let s = value.as_str().ok_or_else(|| bad("color_scheme expects a string"))?;
                let dark = match s {
                    "dark" | "force-dark" | "force_dark" | "prefer-dark" | "prefer_dark" => true,
                    "light" | "default" | "force-light" | "force_light" | "prefer-light" | "prefer_light" => false,
                    _ => return Err(bad("without libadwaita color_scheme is one of: light, dark, default")),
                };
                let previous = prefer_dark(&settings);
                set_prefer_dark(&settings, dark);
                (
                    Prev::PreferDark(previous),
                    Applied {
                        previous: json!({"gtk-application-prefer-dark-theme": previous}),
                        observed: json!({"gtk-application-prefer-dark-theme": prefer_dark(&settings)}),
                    },
                )
            }
        },
        "animations" => {
            let on = value.as_bool().ok_or_else(|| bad("animations expects a boolean"))?;
            let previous = settings.is_gtk_enable_animations();
            settings.set_gtk_enable_animations(on);
            (
                Prev::Animations(previous),
                Applied { previous: json!(previous), observed: json!(settings.is_gtk_enable_animations()) },
            )
        }
        "text_scale" => {
            let scale = value.as_f64().ok_or_else(|| bad("text_scale expects a number"))?;
            if !(MIN_TEXT_SCALE..=MAX_TEXT_SCALE).contains(&scale) {
                return Err(bad(format!("text_scale must be within {MIN_TEXT_SCALE}..={MAX_TEXT_SCALE}")));
            }
            // The scale is a factor of the text size before any override.
            let original = OVERRIDES.with(|o| match o.borrow().get(key) {
                Some(Rec { previous: Prev::XftDpi(d), .. }) => *d,
                _ => settings.gtk_xft_dpi(),
            });
            let base = if original > 0 { original } else { DEFAULT_DPI };
            let dpi = (f64::from(base) * scale).round().clamp(1.0, f64::from(i32::MAX)) as i32;
            settings.set_gtk_xft_dpi(dpi);
            (
                Prev::XftDpi(original),
                Applied {
                    previous: json!({"scale": 1.0, "gtk-xft-dpi": original}),
                    observed: json!({"scale": scale, "gtk-xft-dpi": settings.gtk_xft_dpi()}),
                },
            )
        }
        _ => {
            let dir = match value.as_str() {
                Some("ltr") => gtk::TextDirection::Ltr,
                Some("rtl") => gtk::TextDirection::Rtl,
                _ => return Err(bad("text_direction is \"ltr\" or \"rtl\"")),
            };
            let previous = gtk::Widget::default_direction();
            gtk::Widget::set_default_direction(dir);
            (
                Prev::Direction(previous),
                Applied {
                    previous: json!(direction_name(previous)),
                    observed: json!(direction_name(gtk::Widget::default_direction())),
                },
            )
        }
    };
    // Remember the original only for the first override; later ones (also by
    // other clients) keep it.
    let kept_previous = OVERRIDES.with(|o| {
        let mut o = o.borrow_mut();
        match o.get_mut(key) {
            Some(rec) => {
                if !rec.owners.contains(&conn) {
                    rec.owners.push(conn);
                }
                true
            }
            None => {
                o.insert(key, Rec { previous: prev, owners: vec![conn] });
                false
            }
        }
    });
    Ok(json!({
        "setting": key,
        "previous": applied.previous,
        "earlier_override_active": kept_previous,
        "observed": applied.observed,
    }))
}
