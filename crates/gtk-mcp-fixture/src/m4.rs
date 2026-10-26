//! Fixture additions for M4 (lifecycle, app toolset, `register_state`):
//! crash hooks, a GSettings schema in use, a GResource, app-state closures and
//! `GApplication` open/activate reporting.

use gtk::{gio, glib};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use serde_json::json;
use std::cell::{Cell, RefCell};

pub const SCHEMA_ID: &str = "dev.gtkmcp.Fixture.Prefs";

thread_local! {
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SETTINGS: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
    static OPEN_LABEL: RefCell<Option<glib::WeakRef<gtk::Label>>> = const { RefCell::new(None) };
    static ACTIVATE_LABEL: RefCell<Option<glib::WeakRef<gtk::Label>>> = const { RefCell::new(None) };
    static SETTINGS_LABEL: RefCell<Option<glib::WeakRef<gtk::Label>>> = const { RefCell::new(None) };
}

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

fn set_label(slot: &'static std::thread::LocalKey<RefCell<Option<glib::WeakRef<gtk::Label>>>>, text: &str) {
    if let Some(l) = slot.with(|s| s.borrow().as_ref().and_then(|w| w.upgrade())) {
        l.set_label(text);
    }
}

fn open_text() -> String {
    OPENED.with(|o| {
        let o = o.borrow();
        if o.is_empty() { "open: none".to_string() } else { format!("open: {}", o.join(",")) }
    })
}

fn activate_text() -> String {
    format!("activations: {}", ACTIVATIONS.with(Cell::get))
}

fn settings_text() -> String {
    SETTINGS.with(|s| match s.borrow().as_ref() {
        Some(s) => format!("gsettings: greeting={} volume={}", s.string("greeting"), s.int("volume")),
        None => "gsettings: unavailable".to_string(),
    })
}

fn state_v1() -> serde_json::Value {
    json!({
        "version": 1,
        "activations": ACTIVATIONS.with(Cell::get),
        "opened": OPENED.with(|o| o.borrow().clone()),
    })
}

/// Crash without leaving a core dump behind: SIGSEGV through the default handler.
fn crash_segv() -> ! {
    // SAFETY: deliberately terminates the process; lowering RLIMIT_CORE and resetting the
    // handler (Rust installs a stack-overflow handler that would swallow `raise`) are plain syscalls.
    unsafe {
        let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        eprintln!("fixture-crash: raising SIGSEGV");
        libc::raise(libc::SIGSEGV);
    }
    std::process::abort()
}

/// Application-level setup, before `app.run()`: stable-output option, `open`
/// handling, an activation counter, the GSettings object, the GResource and the
/// `app-state/fixture` closure.
pub fn init_app(app: &adw::Application) {
    app.add_main_option(
        "banner",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Print a stable banner line on stdout and stderr",
        Some("TEXT"),
    );
    app.connect_handle_local_options(|_, dict| {
        if let Some(text) = dict.lookup::<String>("banner").ok().flatten() {
            println!("fixture-banner: {text}");
            eprintln!("fixture-banner: {text}");
        }
        std::ops::ControlFlow::Continue(())
    });
    app.connect_open(|_, files, hint| {
        OPENED.with(|o| o.borrow_mut().extend(files.iter().map(|f| f.uri().to_string())));
        eprintln!("fixture-open: {} file(s), hint '{hint}'", files.len());
        set_label(&OPEN_LABEL, &open_text());
    });
    app.connect_activate(|_| {
        ACTIVATIONS.with(|a| a.set(a.get() + 1));
        eprintln!("fixture-activate: {}", ACTIVATIONS.with(Cell::get));
        set_label(&ACTIVATE_LABEL, &activate_text());
    });

    if let Ok(path) = std::env::var("FIXTURE_GRESOURCE") {
        match gio::Resource::load(&path) {
            Ok(r) => gio::resources_register(&r),
            Err(e) => eprintln!("fixture: cannot load GResource {path}: {e}"),
        }
    }

    // `Settings::new` aborts on a missing schema: look it up first.
    if gio::SettingsSchemaSource::default().is_some_and(|s| s.lookup(SCHEMA_ID, true).is_some()) {
        let settings = gio::Settings::new(SCHEMA_ID);
        settings.connect_changed(None, |_, key| {
            eprintln!("fixture-gsettings-changed: {key}");
            set_label(&SETTINGS_LABEL, &settings_text());
        });
        gtk_mcp_agent::register_settings(&settings);
        SETTINGS.with(|s| *s.borrow_mut() = Some(settings));
    }

    gtk_mcp_agent::register_state("app-state/fixture", state_v1);
    eprintln!("fixture-ready");
}

/// Widgets for the M4 tests, appended to the window's root box.
pub fn extras(root: &gtk::Box, _app: &adw::Application) {
    let open = named(gtk::Label::new(Some(&open_text())), "open-report");
    let activate = named(gtk::Label::new(Some(&activate_text())), "activate-report");
    let settings = named(gtk::Label::new(Some(&settings_text())), "gsettings-report");
    OPEN_LABEL.with(|s| *s.borrow_mut() = Some(open.downgrade()));
    ACTIVATE_LABEL.with(|s| *s.borrow_mut() = Some(activate.downgrade()));
    SETTINGS_LABEL.with(|s| *s.borrow_mut() = Some(settings.downgrade()));
    root.append(&open);
    root.append(&activate);
    root.append(&settings);

    // A panic inside a GTK callback cannot unwind through C: the process aborts (SIGABRT)
    // while the `interact activate` request that triggered it is still in flight.
    let panic_btn = named(gtk::Button::with_label("Crash: panic"), "crash-panic");
    panic_btn.connect_clicked(|_| {
        // SAFETY: lowers RLIMIT_CORE so the test crash leaves no core dump.
        unsafe {
            let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
        }
        eprintln!("fixture-crash: panicking");
        panic!("fixture crash hook: panic in a GTK callback");
    });
    // SIGSEGV 300 ms later, outside any request.
    let segv_btn = named(gtk::Button::with_label("Crash: SIGSEGV soon"), "crash-segv-delayed");
    segv_btn.connect_clicked(|_| {
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), || crash_segv());
    });
    let rereg = named(gtk::Button::with_label("Re-register state"), "reregister-state");
    rereg.connect_clicked(|_| {
        gtk_mcp_agent::register_state("app-state/fixture", || json!({ "version": 2, "replaced": true }));
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&panic_btn);
    row.append(&segv_btn);
    row.append(&rereg);
    root.append(&row);
}
