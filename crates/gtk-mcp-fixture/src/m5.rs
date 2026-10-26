//! Fixture additions for M5 (diagnostics): an animation that keeps the frame
//! clock ticking, GLib log producers, a model to watch, and an emission that
//! the app causes by itself shortly after an activation (`app` origin for
//! `record`). Everything is idle until a test triggers it.

use gtk::glib;
use gtk4 as gtk;
use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

/// Application level, before `app.run()`: `FIXTURE_LOG_WRITER=1` installs a GLib
/// log writer that forwards every record to the agent (`forward_log`) and then
/// prints it like the default writer.
pub fn init_app() {
    if std::env::var_os("FIXTURE_LOG_WRITER").is_some() {
        glib::log_set_writer_func(|level, fields| {
            gtk_mcp_agent::forward_log(level, fields);
            glib::log_writer_default(level, fields)
        });
    }
}

pub fn extras(root: &gtk::Box) {
    // -- frame clock: a drawing area that redraws every frame while "animate" is on.
    let area = named(gtk::DrawingArea::new(), "animated-area");
    area.set_content_width(48);
    area.set_content_height(48);
    let phase = Rc::new(Cell::new(0.0f64));
    {
        let phase = phase.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let p = phase.get();
            cr.set_source_rgb(0.2 + 0.6 * (p.sin() * 0.5 + 0.5), 0.4, 0.8);
            cr.rectangle(0.0, 0.0, f64::from(w) * (p.cos() * 0.4 + 0.6), f64::from(h));
            let _ = cr.fill();
        });
    }
    let animate = named(gtk::ToggleButton::with_label("Animate"), "animate-toggle");
    {
        let area = area.clone();
        let phase = phase.clone();
        let tick: Rc<Cell<Option<gtk::TickCallbackId>>> = Rc::new(Cell::new(None));
        animate.connect_toggled(move |b| {
            if b.is_active() {
                if let Some(old) = tick.take() {
                    old.remove();
                }
                let phase = phase.clone();
                let id = area.add_tick_callback(move |a, _| {
                    phase.set(phase.get() + 0.1);
                    a.queue_draw();
                    glib::ControlFlow::Continue
                });
                tick.set(Some(id));
            } else if let Some(old) = tick.take() {
                old.remove();
            }
        });
    }

    // -- logs: GLib records through the default writer (stderr of a launched app).
    let critical = named(gtk::Button::with_label("Log critical"), "log-critical");
    critical.connect_clicked(|_| glib::g_critical!("FixtureM5", "m5-critical-marker {}", 42));
    let warning = named(gtk::Button::with_label("Log warning"), "log-warning");
    warning.connect_clicked(|_| glib::g_warning!("FixtureM5", "m5-warning-marker"));

    // -- a model to watch.
    let model = gtk::StringList::new(&["one", "two"]);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&gtk::Label::new(None)));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let s = item.item().and_downcast::<gtk::StringObject>().unwrap();
        item.child().and_downcast::<gtk::Label>().unwrap().set_label(&s.string());
    });
    let list = named(gtk::ListView::new(Some(gtk::SingleSelection::new(Some(model.clone()))), Some(factory)), "watch-list");
    let append = named(gtk::Button::with_label("Append item"), "watch-append");
    {
        let model = model.clone();
        append.connect_clicked(move |_| model.append(&format!("item {}", model.n_items() + 1)));
    }

    // -- an emission the app causes itself, after the 300 ms input window: it
    // sets the entry, the password and activates "submit".
    let later = named(gtk::Button::with_label("Emit later"), "emit-later");
    later.connect_clicked(move |_| {
        glib::timeout_add_local_once(Duration::from_millis(900), || {
            let Some(win) = gtk::Window::list_toplevels().into_iter().find_map(|w| w.downcast::<gtk::Window>().ok()) else { return };
            let find = |name: &str| find_by_name(win.upcast_ref(), name);
            if let Some(e) = find("name-entry").and_then(|w| w.dynamic_cast::<gtk::Editable>().ok()) {
                e.set_text("typed-by-the-app");
            }
            if let Some(p) = find("password").and_then(|w| w.dynamic_cast::<gtk::Editable>().ok()) {
                p.set_text("app-secret-9f31");
            }
            if let Some(b) = find("submit").and_then(|w| w.downcast::<gtk::Button>().ok()) {
                b.emit_clicked();
            }
        });
    });

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&animate);
    row.append(&area);
    row.append(&critical);
    row.append(&warning);
    row.append(&append);
    row.append(&later);
    root.append(&row);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(48);
    scroller.set_child(Some(&list));
    root.append(&scroller);
}

fn find_by_name(w: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if w.widget_name() == name {
        return Some(w.clone());
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if let Some(f) = find_by_name(&child, name) {
            return Some(f);
        }
        c = child.next_sibling();
    }
    None
}
