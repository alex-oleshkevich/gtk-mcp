//! gtk4-layer-shell getters resolved with `dlsym(RTLD_DEFAULT)` only when the
//! library is already loaded in the process (plan 6). The agent never links it.
//!
//! gm-66t: `gtk_layer_is_layer_window` alone cannot be trusted to identify a
//! layer window. libgtk4-layer-shell also implements the session-lock API, and
//! `gtk4-session-lock.c` stores its (differently laid out, smaller) lock
//! surface under the *same* `g_object` data key `"wayland_layer_surface"` as
//! `gtk4-layer-shell.c` uses for layer surfaces. `is_layer_window` therefore
//! returns true for session-lock windows, and the `gtk_layer_get_*` getters
//! then read fields past the lock struct, reporting heap garbage (and
//! dereferencing a garbage namespace pointer). [`is_layer_window`] additionally
//! confirms the stored surface really is a layer surface before any getter
//! runs; windows that fail the check get no layer-shell section at all.

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
    if unsafe { f(p.cast()) } == 0 {
        return Some(false);
    }
    Some(surface_is_layer(w))
}

/// Confirm the surface stored under `"wayland_layer_surface"` is a layer
/// surface, not a session-lock surface (see module docs). Only offsets that
/// exist in both structs are read: the shared `xdg_surface_server` vtable at
/// the start, and the two function pointers a layer surface carries right
/// after it (lock surfaces store Wayland proxies there instead).
fn surface_is_layer(w: &gtk::Window) -> bool {
    use glib::translate::ToGlibPtr;
    let p: *mut gtk::ffi::GtkWindow = w.to_glib_none().0;
    // SAFETY: plain qdata lookup; the library owns the stored surface.
    let data = unsafe {
        glib::gobject_ffi::g_object_get_data(p.cast(), c"wayland_layer_surface".as_ptr())
    };
    if data.is_null() {
        return false;
    }
    // SAFETY: `data` is a layer or lock surface struct; both are at least
    // 104 bytes, covering every word read here.
    let word = |byte: usize| unsafe {
        *(data as *const *const c_void).add(byte / std::mem::size_of::<*const c_void>())
    };
    // `layer_surface_make` installs a `window_geometry_set` callback;
    // `lock_surface_make` leaves that vtable slot null.
    if !word(0).is_null() {
        return true;
    }
    // Fallback if a future library stops installing that callback: a layer
    // surface's `remap`/`get_preferred_size` slots (bytes 88/96) point into
    // libgtk4-layer-shell itself, a lock surface's hold heap Wayland proxies.
    let Some(reference) = sym(c"gtk_layer_is_layer_window") else {
        return false;
    };
    in_same_library(word(88), reference) && in_same_library(word(96), reference)
}

fn in_same_library(p: *const c_void, reference: *mut c_void) -> bool {
    if p.is_null() {
        return false;
    }
    // SAFETY: dladdr only inspects the address; DlInfo is plain data.
    unsafe {
        let mut a: libc::Dl_info = std::mem::zeroed();
        let mut b: libc::Dl_info = std::mem::zeroed();
        libc::dladdr(p, &mut a) != 0
            && libc::dladdr(reference, &mut b) != 0
            && !a.dli_fbase.is_null()
            && a.dli_fbase == b.dli_fbase
    }
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
