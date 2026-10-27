//! Spike S4: does a capture-phase `GtkEventControllerLegacy` on a toplevel (and
//! on popovers) observe every real input event, so that emissions of the
//! signals listed in plan 5.6 within 300 ms of one can be attributed to a human?
//!
//! The app prints one JSON object per line on stdout:
//!   {"k":"input","type":..,"t":ms,"x":..,"y":..,"held":n}   (events the controller sees; hover motion counted only)
//!   {"k":"emit","sig":..,"w":name,"attr":"human|app","since_input_ms":..,"held":bool,"t":ms}
//!   {"k":"layout",...}                                       (reply to `layout`)
//! Commands on stdin: `layout`, `popup`, `app` (programmatic emissions of every kind), `quit`.

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::gio;
use gtk4::gio::prelude::*;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_uint, c_void};
use std::io::BufRead;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glib::translate::*;

const WINDOW_MS: u128 = 300;

thread_local! {
    static T0: Instant = Instant::now();
    static LAST_INPUT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Buttons / keys / touches currently held (pressed without a release seen yet).
    static HELD: Cell<i32> = const { Cell::new(0) };
    static CTRL_SEEN: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static MOTION: Cell<u32> = const { Cell::new(0) };
}

fn ms() -> u128 {
    T0.with(|t| t.elapsed().as_millis())
}

fn out(s: String) {
    println!("{s}");
}

fn note_input(ev: &gdk::Event) {
    use gdk::EventType as E;
    let ty = ev.event_type();
    let held_buttons = ev
        .modifier_state()
        .intersects(gdk::ModifierType::BUTTON1_MASK | gdk::ModifierType::BUTTON2_MASK | gdk::ModifierType::BUTTON3_MASK);
    match ty {
        E::MotionNotify if !held_buttons => {
            MOTION.with(|m| m.set(m.get() + 1));
            // Calibration aid for the driver: report positions of hover motion too.
            let (x, y) = ev.position().unwrap_or((-1.0, -1.0));
            out(format!("{{\"k\":\"hover\",\"x\":{x:.1},\"y\":{y:.1}}}"));
            return; // hover is not input that can emit by itself
        }
        E::EnterNotify | E::LeaveNotify | E::FocusChange | E::ProximityIn | E::ProximityOut | E::GrabBroken => return,
        E::ButtonPress | E::KeyPress | E::TouchBegin => HELD.with(|h| h.set(h.get() + 1)),
        E::ButtonRelease | E::KeyRelease | E::TouchEnd | E::TouchCancel => HELD.with(|h| h.set((h.get() - 1).max(0))),
        _ => {}
    }
    LAST_INPUT.with(|l| l.set(Some(Instant::now())));
    let (x, y) = ev.position().unwrap_or((-1.0, -1.0));
    out(format!(
        "{{\"k\":\"input\",\"type\":\"{ty:?}\",\"t\":{},\"x\":{x:.1},\"y\":{y:.1},\"held\":{}}}",
        ms(),
        HELD.with(Cell::get)
    ));
}

/// Capture-phase legacy controller; one per native (toplevel or popover).
fn install_controller(w: &gtk::Widget) {
    let key = w.as_ptr() as usize;
    if CTRL_SEEN.with(|s| s.borrow().contains(&key)) {
        return;
    }
    CTRL_SEEN.with(|s| s.borrow_mut().push(key));
    let c = gtk::EventControllerLegacy::new();
    c.set_propagation_phase(gtk::PropagationPhase::Capture);
    c.connect_event(|_, ev| {
        note_input(ev);
        glib::Propagation::Proceed
    });
    w.add_controller(c);
    out(format!("{{\"k\":\"controller\",\"on\":\"{}\"}}", w.type_().name()));
}

fn attribute() -> (&'static str, i64, bool) {
    let held = HELD.with(Cell::get) > 0;
    let since = LAST_INPUT.with(Cell::get).map(|t| t.elapsed().as_millis() as i64).unwrap_or(-1);
    let human = held || (since >= 0 && (since as u128) <= WINDOW_MS);
    (if human { "human" } else { "app" }, since, held)
}

fn widget_name(o: &glib::Object) -> String {
    match o.downcast_ref::<gtk::Widget>() {
        Some(w) => {
            let n = w.widget_name();
            if n.is_empty() || n.starts_with("Gtk") { w.type_().name().to_string() } else { n.to_string() }
        }
        None => o.type_().name().to_string(),
    }
}

unsafe extern "C" fn hook(
    ihint: *mut glib::gobject_ffi::GSignalInvocationHint,
    n: c_uint,
    vals: *const glib::gobject_ffi::GValue,
    _data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if n == 0 {
        return 1;
    }
    // SAFETY: first value of an emission is the instance; `Value` is transparent over `GValue`.
    let inst = unsafe { &*(vals as *const glib::Value) };
    let Ok(Some(obj)) = inst.get::<Option<glib::Object>>() else { return 1 };
    let sig = unsafe { CStr::from_ptr(glib::gobject_ffi::g_signal_name((*ihint).signal_id)) }.to_string_lossy().into_owned();
    let (attr, since, held) = attribute();
    out(format!(
        "{{\"k\":\"emit\",\"sig\":\"{}::{sig}\",\"w\":\"{}\",\"attr\":\"{attr}\",\"since_input_ms\":{since},\"held\":{held},\"t\":{}}}",
        obj.type_().name(),
        widget_name(&obj),
        ms()
    ));
    1
}

fn add_hook(type_name: &str, signal: &str) {
    let Some(t) = glib::Type::from_name(type_name) else {
        out(format!("{{\"k\":\"warn\",\"msg\":\"no type {type_name}\"}}"));
        return;
    };
    // SAFETY: plain GObject type-system calls; the class reference is kept for the process lifetime.
    unsafe {
        let _ = glib::gobject_ffi::g_type_class_ref(t.into_glib());
        let id = glib::gobject_ffi::g_signal_lookup(signal.to_glib_none().0, t.into_glib());
        if id == 0 {
            out(format!("{{\"k\":\"warn\",\"msg\":\"no signal {type_name}::{signal}\"}}"));
            return;
        }
        glib::gobject_ffi::g_signal_add_emission_hook(id, 0, Some(hook), std::ptr::null_mut::<c_void>(), None);
    }
}

fn build(app: &gtk::Application) {
    let win = gtk::ApplicationWindow::builder().application(app).title("s4").default_width(520).default_height(520).decorated(false).build();
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let entry = gtk::Entry::builder().name("entry").build();
    let btn = gtk::Button::builder().label("Go").name("btn").build();
    let sw = gtk::Switch::builder().name("sw").halign(gtk::Align::Start).build();
    let check = gtk::CheckButton::builder().label("check").name("check").build();
    let toggle = gtk::ToggleButton::builder().label("toggle").name("toggle").build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_widget_name("scale");
    let spin = gtk::SpinButton::with_range(0.0, 1000.0, 1.0);
    spin.set_widget_name("spin");
    let exp = gtk::Expander::builder().label("exp").name("exp").build();
    exp.set_child(Some(&gtk::Label::new(Some("inside"))));
    let pad = gtk::Box::builder().name("pad").height_request(60).build();
    pad.add_css_class("card");
    let pad_label = gtk::Label::new(Some("pad"));
    pad.append(&pad_label);
    let pad_clicks = Rc::new(Cell::new(0u32));
    let g = gtk::GestureClick::new();
    {
        let c = pad_clicks.clone();
        let l = pad_label.clone();
        g.connect_pressed(move |_, _, _, _| {
            c.set(c.get() + 1);
            l.set_text(&format!("pad {}", c.get()));
        });
    }
    pad.add_controller(g);
    // Popover menu with a GAction.
    let acts = gio::SimpleActionGroup::new();
    let act = gio::SimpleAction::new("pick", None);
    act.connect_activate(|_, _| {});
    acts.add_action(&act);
    win.insert_action_group("m", Some(&acts));
    let menu = gio::Menu::new();
    menu.append(Some("Pick"), Some("m.pick"));
    let mb = gtk::MenuButton::builder().label("Menu").name("menubtn").menu_model(&menu).build();
    for w in [
        entry.upcast_ref::<gtk::Widget>(),
        btn.upcast_ref(),
        sw.upcast_ref(),
        check.upcast_ref(),
        toggle.upcast_ref(),
        scale.upcast_ref(),
        spin.upcast_ref(),
        exp.upcast_ref(),
        pad.upcast_ref(),
        mb.upcast_ref(),
    ] {
        bx.append(w);
    }
    win.set_child(Some(&bx));
    install_controller(win.upcast_ref());
    // Popovers are separate natives: they need their own controller.
    if let Some(pop) = mb.popover() {
        install_controller(pop.upcast_ref());
    }

    for (t, s) in [
        ("GtkButton", "clicked"),
        ("GtkToggleButton", "toggled"),
        ("GtkCheckButton", "toggled"),
        ("GtkSwitch", "state-set"),
        ("GtkEditable", "changed"),
        ("GtkExpander", "activate"),
        ("GtkRange", "value-changed"),
        ("GtkSpinButton", "value-changed"),
        ("GSimpleAction", "activate"),
        ("GSimpleAction", "change-state"),
    ] {
        add_hook(t, s);
    }

    let (tx, rx) = async_channel::unbounded::<String>();
    std::thread::spawn(move || {
        for l in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send_blocking(l).is_err() {
                break;
            }
        }
        let _ = tx.send_blocking("quit".into());
    });
    let app2 = app.clone();
    let wl = (entry, btn, sw, check, toggle, scale, spin, exp, act, win.clone(), mb, pad);
    glib::spawn_future_local(async move {
        while let Ok(cmd) = rx.recv().await {
            match cmd.as_str() {
                "quit" => {
                    app2.quit();
                    break;
                }
                "layout" => {
                    let (e, b, s, c, t, sc, sp, ex, _a, w, m, p) = &wl;
                    let mut parts = Vec::new();
                    for x in [
                        e.upcast_ref::<gtk::Widget>(),
                        b.upcast_ref(),
                        s.upcast_ref(),
                        c.upcast_ref(),
                        t.upcast_ref(),
                        sc.upcast_ref(),
                        sp.upcast_ref(),
                        ex.upcast_ref(),
                        m.upcast_ref(),
                        p.upcast_ref(),
                    ] {
                        let bd = x.compute_bounds(w).unwrap();
                        parts.push(format!("\"{}\":[{:.1},{:.1},{:.1},{:.1}]", x.widget_name(), bd.x(), bd.y(), bd.width(), bd.height()));
                    }
                    out(format!("{{\"k\":\"layout\",\"win\":[{},{}],{}}}", w.width(), w.height(), parts.join(",")));
                }
                "popup" => wl.10.popup(),
                "app" => {
                    let (e, b, s, c, t, sc, sp, ex, a, _w, _m, _p) = &wl;
                    out("{\"k\":\"app_begin\"}".into());
                    // Wait out any human window left over from the driver's last action.
                    glib::timeout_future(Duration::from_millis(800)).await;
                    b.emit_clicked();
                    e.set_text("from-app");
                    s.set_active(!s.is_active());
                    c.set_active(!c.is_active());
                    t.set_active(!t.is_active());
                    sc.set_value(sc.value() + 5.0);
                    sp.set_value(sp.value() + 3.0);
                    ex.set_expanded(!ex.is_expanded());
                    a.activate(None);
                    out("{\"k\":\"app_end\"}".into());
                }
                _ => {}
            }
        }
    });
    win.present();
    out("{\"k\":\"ready\"}".into());
}

fn main() {
    gtk::init().expect("gtk init");
    let app = gtk::Application::builder().application_id("org.gtkmcp.S4").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(build);
    app.run_with_args::<&str>(&[]);
}
