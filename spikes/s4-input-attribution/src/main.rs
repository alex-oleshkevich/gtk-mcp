//! Spike S4: does a capture-phase GtkEventControllerLegacy on a toplevel (and on
//! popovers) see real input, and does a 300 ms window separate human from app
//! emissions? Prints `L <t_ms> <event>` for every legacy-controller event and
//! `E <t_ms> <emission> <delta_ms>` for emissions; `delta` is the time since the
//! last legacy *input* event (key/button/scroll/touch; motion is ignored).
use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;
use std::cell::Cell;
use std::rc::Rc;

fn now_ms() -> f64 {
    glib::monotonic_time() as f64 / 1000.0
}

fn attach_legacy(native: &impl IsA<gtk::Widget>, tag: &'static str, last: Rc<Cell<f64>>) {
    let c = gtk::EventControllerLegacy::new();
    c.set_propagation_phase(gtk::PropagationPhase::Capture);
    c.connect_event(move |_, ev| {
        use gtk::gdk::EventType as T;
        let t = ev.event_type();
        let input = matches!(
            t,
            T::KeyPress | T::KeyRelease | T::ButtonPress | T::ButtonRelease | T::Scroll | T::TouchBegin | T::TouchEnd | T::TouchUpdate
        );
        if input {
            last.set(now_ms());
        }
        if std::env::var_os("S4_ALL").is_some() || !matches!(t, T::MotionNotify | T::EnterNotify | T::LeaveNotify | T::FocusChange) {
            println!("L {:.1} {tag} {:?}", now_ms(), t);
        }
        glib::Propagation::Proceed
    });
    native.add_controller(c);
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id("dev.gtkmcp.S4").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(|app| {
        let last = Rc::new(Cell::new(-1e9));
        let emit = {
            let last = last.clone();
            move |what: &str| println!("E {:.1} {what} {:.1}", now_ms(), now_ms() - last.get())
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let entry = gtk::Entry::new();
        entry.set_widget_name("entry");
        {
            let e = emit.clone();
            entry.connect_changed(move |_| e("entry.changed"));
        }
        let button = gtk::Button::with_label("Click me");
        button.set_widget_name("button");
        {
            let e = emit.clone();
            button.connect_clicked(move |_| e("button.clicked"));
        }
        let sw = gtk::Switch::new();
        {
            let e = emit.clone();
            sw.connect_state_set(move |_, _| {
                e("switch.state-set");
                glib::Propagation::Proceed
            });
        }
        let menu = gio::Menu::new();
        menu.append(Some("Do it"), Some("win.doit"));
        let mb = gtk::MenuButton::new();
        mb.set_label("Menu");
        mb.set_menu_model(Some(&menu));
        root.append(&entry);
        root.append(&button);
        root.append(&sw);
        root.append(&mb);
        let win = gtk::ApplicationWindow::builder().application(app).default_width(400).default_height(300).child(&root).build();
        let doit = gio::SimpleAction::new("doit", None);
        {
            let e = emit.clone();
            doit.connect_activate(move |_, _| e("action.doit.activate"));
        }
        win.add_action(&doit);
        attach_legacy(&win, "window", last.clone());
        // The menu button's popover is a separate native.
        if let Some(p) = mb.popover() {
            attach_legacy(&p, "popover", last.clone());
        }
        // App-originated emissions with no input around them.
        {
            let button = button.clone();
            let entry = entry.clone();
            let e = emit.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                e("-- app emits click and text");
                button.emit_clicked();
                entry.set_text("from-app");
            });
        }
        win.present();
        println!("READY");
        {
            let win2 = win.clone();
            let ws: Vec<(&'static str, gtk::Widget)> =
                vec![("entry", entry.clone().upcast()), ("button", button.clone().upcast()), ("switch", sw.clone().upcast()), ("menu", mb.clone().upcast())];
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                for (n, w) in &ws {
                    if let Some(b) = w.compute_bounds(&win2) {
                        println!("B {n} {:.0} {:.0} {:.0} {:.0}", b.x(), b.y(), b.width(), b.height());
                    }
                }
            });
        }
    });
    app.run()
}
