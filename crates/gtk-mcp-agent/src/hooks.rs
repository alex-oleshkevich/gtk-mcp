//! Signal emission hooks (`g_signal_add_emission_hook`) shared by `watch` and
//! `record`. One GLib hook per signal id fans out to any number of listeners;
//! the GLib hook is removed with the last listener. Hooks run at the start of
//! an emission, before the instance's handlers, and never change its outcome.
//!
//! `GObject::notify` is flagged `G_SIGNAL_NO_HOOKS` and cannot be hooked
//! (`list_signals` reports it); notifications use `connect_notify_local`.

use glib::translate::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Called with the emitting instance and all parameter values (instance first).
pub type Listener = Rc<dyn Fn(&glib::Object, &[glib::Value])>;

struct Entry {
    hook_id: std::ffi::c_ulong,
    signal_id: u32,
    listeners: Vec<(u64, Listener)>,
}

thread_local! {
    static HOOKS: RefCell<HashMap<u32, Entry>> = RefCell::new(HashMap::new());
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

/// Identifies one listener for [`remove`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId {
    signal: u32,
    id: u64,
}

unsafe extern "C" fn trampoline(
    ihint: *mut glib::gobject_ffi::GSignalInvocationHint,
    n_values: std::ffi::c_uint,
    values: *const glib::gobject_ffi::GValue,
    _data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    // SAFETY: GLib passes a valid hint and `n_values` values; `glib::Value` is a
    // transparent wrapper of `GValue`. The values are only borrowed for the call.
    let (signal_id, vals) = unsafe {
        ((*ihint).signal_id, std::slice::from_raw_parts(values as *const glib::Value, n_values as usize))
    };
    if let Some(instance) = vals.first().and_then(|v| v.get::<glib::Object>().ok()) {
        // Clone out of the registry: listeners may add or remove listeners.
        let listeners: Vec<Listener> =
            HOOKS.with(|h| h.borrow().get(&signal_id).map(|e| e.listeners.iter().map(|(_, l)| l.clone()).collect()).unwrap_or_default());
        for l in listeners {
            l(&instance, vals);
        }
    }
    true.into_glib() // stay installed
}

/// Hook `signal` of `type_` (or an ancestor/interface declaring it). `None`
/// when the signal does not exist or cannot be hooked.
pub fn add(type_: glib::Type, signal: &str, listener: Listener) -> Option<ListenerId> {
    // `static_type()` only registers the type. Signals are registered by class
    // (or default interface) initialization, even before the first instance.
    let _class = glib::Class::<glib::Object>::from_type(type_);
    let interface = if type_.is_a(glib::Type::INTERFACE) {
        // SAFETY: `type_` is an interface; balance the ref after the lookup.
        unsafe { glib::gobject_ffi::g_type_default_interface_ref(type_.into_glib()) }
    } else {
        std::ptr::null_mut()
    };
    let sid = glib::subclass::SignalId::lookup(signal, type_);
    if !interface.is_null() {
        // SAFETY: this is the reference acquired above.
        unsafe { glib::gobject_ffi::g_type_default_interface_unref(interface) };
    }
    let sid = sid?;
    let q = sid.query();
    if q.flags().contains(glib::SignalFlags::NO_HOOKS) {
        return None;
    }
    let signal_id: u32 = sid.into_glib();
    let id = NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    });
    HOOKS.with(|h| {
        let mut h = h.borrow_mut();
        if let Some(e) = h.get_mut(&signal_id) {
            e.listeners.push((id, listener));
            return Some(());
        }
        // SAFETY: the hook function and its (null) data stay valid for the life of the process.
        let hook_id = unsafe {
            glib::gobject_ffi::g_signal_add_emission_hook(signal_id, 0, Some(trampoline), std::ptr::null_mut(), None)
        };
        if hook_id == 0 {
            return None;
        }
        h.insert(signal_id, Entry { hook_id, signal_id, listeners: vec![(id, listener)] });
        Some(())
    })?;
    Some(ListenerId { signal: signal_id, id })
}

/// Remove a listener; the GLib hook goes with the last one.
pub fn remove(l: ListenerId) {
    HOOKS.with(|h| {
        let mut h = h.borrow_mut();
        let Some(e) = h.get_mut(&l.signal) else { return };
        e.listeners.retain(|(i, _)| *i != l.id);
        if e.listeners.is_empty() {
            let e = h.remove(&l.signal).expect("entry present");
            // SAFETY: `hook_id` was returned by `g_signal_add_emission_hook` for this signal.
            unsafe { glib::gobject_ffi::g_signal_remove_emission_hook(e.signal_id, e.hook_id) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;
    use glib::subclass::prelude::*;
    use std::sync::LazyLock;

    mod imp {
        use super::*;

        #[derive(Default)]
        pub struct ColdSignal;

        #[glib::object_subclass]
        impl ObjectSubclass for ColdSignal {
            const NAME: &'static str = "GtkMcpIssue31ColdSignal";
            type Type = super::ColdSignal;
            type ParentType = glib::Object;
        }

        impl ObjectImpl for ColdSignal {
            fn signals() -> &'static [glib::subclass::Signal] {
                static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> =
                    LazyLock::new(|| vec![glib::subclass::Signal::builder("cold-signal").build()]);
                &SIGNALS
            }
        }
    }

    glib::wrapper! {
        pub struct ColdSignal(ObjectSubclass<imp::ColdSignal>);
    }

    #[test]
    fn hook_initializes_class_before_first_instance() {
        let ty = ColdSignal::static_type();
        assert!(glib::subclass::SignalId::lookup("cold-signal", ty).is_none());
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        let id = add(ty, "cold-signal", Rc::new(move |_, _| seen.set(seen.get() + 1)))
            .expect("hook initializes the previously unused class");
        let obj: ColdSignal = glib::Object::new();
        obj.emit_by_name::<()>("cold-signal", &[]);
        assert_eq!(calls.get(), 1);
        remove(id);
        obj.emit_by_name::<()>("cold-signal", &[]);
        assert_eq!(calls.get(), 1, "removing the listener still removes its hook");
    }
}
