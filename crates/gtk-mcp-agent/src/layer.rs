//! gtk4-layer-shell getters resolved with `dlsym(RTLD_DEFAULT)` only when the
//! library is already loaded in the process (plan 6). The agent never links it.

use gtk4 as gtk;
use serde_json::{Value, json};
use std::ffi::{CStr, c_char, c_int, c_void};

fn sym(name: &CStr) -> Option<*mut c_void> {
    // SAFETY: plain lookup in the global namespace.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

type WinInt = unsafe extern "C" fn(*mut c_void) -> c_int;
type WinEdgeInt = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type WinStr = unsafe extern "C" fn(*mut c_void) -> *const c_char;

/// Is gtk4-layer-shell loaded into this process?
pub fn loaded() -> bool {
    sym(c"gtk_layer_is_layer_window").is_some()
}

fn is_layer_window(w: &gtk::Window) -> Option<bool> {
    use glib::translate::ToGlibPtr;
    let f: WinInt = unsafe { std::mem::transmute(sym(c"gtk_layer_is_layer_window")?) };
    let p: *mut gtk::ffi::GtkWindow = w.to_glib_none().0;
    Some(unsafe { f(p.cast()) } != 0)
}

/// The window's namespace when it is a layer-shell window.
pub fn namespace(w: &gtk::Window) -> Option<String> {
    use glib::translate::ToGlibPtr;
    if is_layer_window(w) != Some(true) {
        return None;
    }
    let f: WinStr = unsafe { std::mem::transmute(sym(c"gtk_layer_get_namespace")?) };
    let p: *mut gtk::ffi::GtkWindow = w.to_glib_none().0;
    let s = unsafe { f(p.cast()) };
    (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned())
}

/// Layer-shell configuration, or `None` when the library is not loaded or the
/// window is not a layer window.
pub fn config(w: &gtk::Window) -> Option<Value> {
    use glib::translate::ToGlibPtr;
    if is_layer_window(w) != Some(true) {
        return None;
    }
    let p: *mut gtk::ffi::GtkWindow = w.to_glib_none().0;
    let p: *mut c_void = p.cast();
    let int = |name: &CStr| -> Option<c_int> {
        let f: WinInt = unsafe { std::mem::transmute(sym(name)?) };
        Some(unsafe { f(p) })
    };
    let edge = |name: &CStr, e: c_int| -> Option<c_int> {
        let f: WinEdgeInt = unsafe { std::mem::transmute(sym(name)?) };
        Some(unsafe { f(p, e) })
    };
    let layer = match int(c"gtk_layer_get_layer")? {
        0 => "background",
        1 => "bottom",
        2 => "top",
        3 => "overlay",
        _ => "unknown",
    };
    let keyboard = match int(c"gtk_layer_get_keyboard_mode") {
        Some(0) => "none",
        Some(1) => "exclusive",
        Some(2) => "on_demand",
        _ => "unknown",
    };
    let edges = [("left", 0), ("right", 1), ("top", 2), ("bottom", 3)];
    let anchors: Vec<&str> = edges
        .iter()
        .filter(|(_, e)| edge(c"gtk_layer_get_anchor", *e) == Some(1))
        .map(|(n, _)| *n)
        .collect();
    let margins: serde_json::Map<String, Value> = edges
        .iter()
        .filter_map(|(n, e)| edge(c"gtk_layer_get_margin", *e).map(|m| (n.to_string(), json!(m))))
        .collect();
    Some(json!({
        "layer": layer,
        "namespace": namespace(w),
        "anchors": anchors,
        "margins": margins,
        "exclusive_zone": int(c"gtk_layer_get_exclusive_zone"),
        "keyboard_mode": keyboard,
    }))
}
