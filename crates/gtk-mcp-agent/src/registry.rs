//! Object registry (plan 4.3). Ids live in object qdata; the registry holds
//! only weak references, so a reused address can never inherit an old ref and
//! taking a snapshot never keeps a widget alive.

use glib::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjState {
    /// Inside a root (or a root itself).
    Live,
    /// Alive, `gtk_widget_get_root` is NULL and not itself a root.
    Detached,
}

impl ObjState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Detached => "detached",
        }
    }
}

pub fn widget_state(w: &gtk::Widget) -> ObjState {
    if w.root().is_some() || w.is::<gtk::Root>() { ObjState::Live } else { ObjState::Detached }
}

#[derive(Default)]
pub struct Registry {
    map: RefCell<HashMap<String, glib::WeakRef<glib::Object>>>,
    next_w: Cell<u64>,
    next_o: Cell<u64>,
}

fn ref_quark() -> glib::Quark {
    glib::Quark::from_str("gtk-mcp-ref")
}

impl Registry {
    /// The ref of `obj`, assigning one on first sight (`w<N>` widgets, `o<N>` others).
    pub fn ref_for(&self, obj: &impl IsA<glib::Object>) -> String {
        let obj = obj.upcast_ref::<glib::Object>();
        // SAFETY: the quark is only ever used with `String` values.
        if let Some(p) = unsafe { obj.qdata::<String>(ref_quark()) } {
            return unsafe { p.as_ref() }.clone();
        }
        let id = if obj.is::<gtk::Widget>() {
            let n = self.next_w.get() + 1;
            self.next_w.set(n);
            format!("w{n}")
        } else {
            let n = self.next_o.get() + 1;
            self.next_o.set(n);
            format!("o{n}")
        };
        unsafe { obj.set_qdata(ref_quark(), id.clone()) };
        self.map.borrow_mut().insert(id.clone(), obj.downgrade());
        id
    }

    /// Resolve a ref. Never-issued -> `not_found`; finalized -> `stale_reference`.
    pub fn resolve(&self, r: &str) -> Result<glib::Object, AgentError> {
        let valid = r.len() > 1
            && matches!(r.as_bytes()[0], b'w' | b'o')
            && r[1..].bytes().all(|b| b.is_ascii_digit());
        if !valid {
            return Err(AgentError::invalid_params(format!("'{r}' is not a reference (expected w<N> or o<N>)")));
        }
        let map = self.map.borrow();
        match map.get(r) {
            None => Err(AgentError::new(ErrorCode::NotFound, format!("no object has been given the reference '{r}'"))),
            Some(weak) => weak.upgrade().ok_or_else(|| {
                AgentError::new(ErrorCode::StaleReference, format!("'{r}' refers to an object that no longer exists"))
            }),
        }
    }

    /// Resolve a ref that must be a widget.
    pub fn resolve_widget(&self, r: &str) -> Result<gtk::Widget, AgentError> {
        self.resolve(r)?
            .downcast::<gtk::Widget>()
            .map_err(|o| AgentError::new(ErrorCode::UnsupportedOperation, format!("'{r}' is a {} , not a widget", o.type_().name())))
    }

    /// Number of ids issued (live or not); for tests.
    pub fn issued(&self) -> usize {
        self.map.borrow().len()
    }
}
