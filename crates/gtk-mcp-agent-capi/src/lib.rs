//! C ABI of the embedded agent (`docs/embedding.md`, "Other languages").
//!
//! The library links GTK dynamically (`libgtk-4.so` is `NEEDED`, never
//! embedded), so it binds to the GTK the host process already loaded. It never
//! links libadwaita or gtk4-layer-shell. Every function must be called on the
//! host's GTK main thread; the agent is active once `gtk_mcp_agent_attach`
//! has been called. The header is `include/gtk-mcp-agent.h`; `GtkMcp-1.gir`
//! describes the same functions for GObject introspection.

use glib::translate::{FromGlibPtrBorrow, IntoGlib};
use glib::types::StaticType;
use gtk4 as gtk;
use std::ffi::{c_char, c_void, CStr};

/// `char *(*GtkMcpStateFunc) (gpointer user_data)`: returns a newly allocated
/// (`g_malloc`) JSON document, or NULL for `null`.
pub type GtkMcpStateFunc = Option<unsafe extern "C" fn(user_data: *mut c_void) -> *mut c_char>;
pub type GDestroyNotifyFn = Option<unsafe extern "C" fn(data: *mut c_void)>;

fn str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string (documented in the header).
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// The precondition GTK's own API checks with `g_return_if_fail (GTK_IS_…)`:
/// non-NULL `p` is an instance of `T` (a class or an interface). Otherwise a
/// critical names `func` and the expected type, and the call does nothing.
fn instance_of<T: StaticType>(func: &str, arg: &str, p: *const c_void) -> bool {
    let ty = T::static_type();
    // SAFETY: `p` is non-NULL and, per the header, a GTypeInstance; the check
    // only reads its class.
    let ok = unsafe { glib::gobject_ffi::g_type_check_instance_is_a(p as *mut _, ty.into_glib()) } != 0;
    if !ok {
        glib::g_critical!("GtkMcp", "{func}: assertion '{arg} is a {}' failed", ty.name());
    }
    ok
}

/// Attach the agent to `app` (the C form of `gtk_mcp_agent::attach`).
///
/// # Safety
/// `app` must be a valid `GtkApplication*` (or NULL, which is ignored) and the
/// call must happen on the GTK main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_mcp_agent_attach(app: *mut gtk::ffi::GtkApplication) {
    if app.is_null() || !instance_of::<gtk::Application>("gtk_mcp_agent_attach", "app", app.cast()) {
        return;
    }
    // SAFETY: `app` is a live GtkApplication. Borrowed: no refcount change, and in
    // particular no `g_object_ref_sink`, which would consume a floating reference
    // the caller still owns (C callers pass freshly created, floating widgets).
    let app = unsafe { gtk::Application::from_glib_borrow(app) };
    gtk_mcp_agent::attach(&*app);
}

/// Mark `object` (normally a widget) as secret; see plan 4.4.
///
/// # Safety
/// `object` must be a valid `GObject*` (or NULL, ignored); main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_mcp_agent_mark_secret(object: *mut glib::gobject_ffi::GObject) {
    if object.is_null() || !instance_of::<glib::Object>("gtk_mcp_agent_mark_secret", "object", object.cast()) {
        return;
    }
    // SAFETY: valid GObject, borrowed (see `gtk_mcp_agent_attach`).
    let obj = unsafe { glib::Object::from_glib_borrow(object) };
    gtk_mcp_agent::mark_secret(&*obj);
}

/// Make the `GActionGroup` inserted on `widget` under `prefix` listable by the agent.
///
/// # Safety
/// `widget`/`group` valid instances (NULL ignored), `prefix` NUL-terminated; main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_mcp_agent_register_action_group(
    widget: *mut gtk::ffi::GtkWidget,
    prefix: *const c_char,
    group: *mut gio::ffi::GActionGroup,
) {
    let Some(prefix) = str_arg(prefix) else { return };
    if widget.is_null() || group.is_null() {
        return;
    }
    const F: &str = "gtk_mcp_agent_register_action_group";
    if !instance_of::<gtk::Widget>(F, "widget", widget.cast()) || !instance_of::<gio::ActionGroup>(F, "group", group.cast()) {
        return;
    }
    // SAFETY: valid instances, borrowed (a floating widget must stay floating).
    let (widget, group) = unsafe { (gtk::Widget::from_glib_borrow(widget), gio::ActionGroup::from_glib_borrow(group)) };
    gtk_mcp_agent::register_action_group(&*widget, prefix, &*group);
}

/// Tell the agent about a `GSettings` object the app uses.
///
/// # Safety
/// `settings` a valid `GSettings*` (NULL ignored); main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_mcp_agent_register_settings(settings: *mut gio::ffi::GSettings) {
    if settings.is_null() || !instance_of::<gio::Settings>("gtk_mcp_agent_register_settings", "settings", settings.cast()) {
        return;
    }
    // SAFETY: valid GSettings, borrowed.
    let settings = unsafe { gio::Settings::from_glib_borrow(settings) };
    gtk_mcp_agent::register_settings(&*settings);
}

struct StateCallback {
    func: unsafe extern "C" fn(*mut c_void) -> *mut c_char,
    user_data: *mut c_void,
    destroy: GDestroyNotifyFn,
}

impl Drop for StateCallback {
    fn drop(&mut self) {
        if let Some(d) = self.destroy {
            // SAFETY: the contract of `GDestroyNotify`; runs once.
            unsafe { d(self.user_data) };
        }
    }
}

impl StateCallback {
    fn call(&self) -> serde_json::Value {
        // SAFETY: the callback contract is documented in the header.
        let p = unsafe { (self.func)(self.user_data) };
        if p.is_null() {
            return serde_json::Value::Null;
        }
        // SAFETY: the callback returns a NUL-terminated g_malloc'd string we now own.
        let text = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { glib::ffi::g_free(p.cast()) };
        serde_json::from_str(&text)
            .unwrap_or_else(|e| serde_json::json!({ "error": format!("the state callback returned invalid JSON: {e}") }))
    }
}

/// Expose `callback`'s JSON as the read-only resource `app-state/<name>`.
/// `destroy` runs when the registration is replaced, or at once when the agent
/// is inactive, `callback` is NULL or `name` is NULL/invalid. It may call back
/// into this API.
///
/// # Safety
/// `name` NUL-terminated; `callback` valid for the lifetime of the registration; main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_mcp_agent_register_state(
    name: *const c_char,
    callback: GtkMcpStateFunc,
    user_data: *mut c_void,
    destroy: GDestroyNotifyFn,
) {
    let Some(func) = callback else {
        if let Some(d) = destroy {
            // SAFETY: nothing else owns `user_data`.
            unsafe { d(user_data) };
        }
        return;
    };
    let cb = StateCallback { func, user_data, destroy };
    match str_arg(name) {
        Some(name) => gtk_mcp_agent::register_state(name, move || cb.call()),
        None => drop(cb),
    }
}
