//! Spike S2: does the frame clock's `after-paint` still fire after
//! `queue_draw()` for windows that are occluded, unfocused, minimized, on a
//! non-active workspace, or hidden?
//!
//! Driven over stdin (one command per line), so an outside script can
//! rearrange the compositor between measurements:
//!
//!   new <name> [w h]           create + present a window titled `s2-<name>`
//!   measure <name> <n> <gap>   n times: change the label, queue_draw(), wait up
//!                              to 1000 ms for after-paint; wait <gap> ms between
//!   hide|show|minimize|unminimize|present <name>
//!   status <name>              realized / mapped / visible / is_active / frame clock
//!   quit
//!
//! Output: one JSON object per line on stdout.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::BufRead;
use std::rc::Rc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_millis(1000);

struct Win {
    window: gtk::Window,
    label: gtk::Label,
    counter: Cell<u32>,
}

fn out(s: String) {
    println!("{s}");
}

async fn measure(w: Rc<Win>, name: String, n: u32, gap: u64) {
    for i in 0..n {
        let Some(clock) = w.window.frame_clock() else {
            out(format!(
                r#"{{"event":"measure","name":"{name}","i":{i},"after_paint":false,"latency_ms":null,"note":"no frame clock (window not realized)"}}"#
            ));
            glib::timeout_future(Duration::from_millis(gap)).await;
            continue;
        };
        let (tx, rx) = async_channel::unbounded::<Instant>();
        let updates = Rc::new(Cell::new(0u32));
        let u = updates.clone();
        let h_update = clock.connect_update(move |_| u.set(u.get() + 1));
        let h_paint = clock.connect_after_paint(move |_| {
            let _ = tx.try_send(Instant::now());
        });
        let mapped = w.window.is_mapped();
        w.counter.set(w.counter.get() + 1);
        w.label.set_text(&format!("s2 {name} #{}", w.counter.get()));
        let t0 = Instant::now();
        w.label.queue_draw();
        let got = glib::future_with_timeout(TIMEOUT, rx.recv()).await;
        let (received, latency) = match got {
            Ok(Ok(t)) => (true, Some(t.duration_since(t0).as_secs_f64() * 1000.0)),
            _ => (false, None),
        };
        clock.disconnect(h_update);
        clock.disconnect(h_paint);
        out(format!(
            r#"{{"event":"measure","name":"{name}","i":{i},"after_paint":{received},"latency_ms":{},"update_events":{},"mapped":{mapped},"is_active":{}}}"#,
            latency.map_or("null".to_string(), |l| format!("{l:.1}")),
            updates.get(),
            w.window.is_active()
        ));
        glib::timeout_future(Duration::from_millis(gap)).await;
    }
    out(format!(r#"{{"event":"measure_done","name":"{name}"}}"#));
}

fn handle(app: &gtk::Application, wins: &Rc<RefCell<HashMap<String, Rc<Win>>>>, line: &str) {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let Some(cmd) = parts.first() else { return };
    let get = |idx: usize| parts.get(idx).map(|s| s.to_string()).unwrap_or_default();
    let win = |name: &str| wins.borrow().get(name).cloned();
    match *cmd {
        "new" => {
            let name = get(1);
            let (w, h) = (get(2).parse().unwrap_or(400), get(3).parse().unwrap_or(200));
            let window = gtk::Window::builder().application(app).title(format!("s2-{name}")).default_width(w).default_height(h).build();
            let label = gtk::Label::new(Some(&format!("s2 {name}")));
            window.set_child(Some(&label));
            window.present();
            wins.borrow_mut().insert(name.clone(), Rc::new(Win { window, label, counter: Cell::new(0) }));
            out(format!(r#"{{"event":"new","name":"{name}"}}"#));
        }
        "measure" => {
            if let Some(w) = win(&get(1)) {
                glib::spawn_future_local(measure(w, get(1), get(2).parse().unwrap_or(5), get(3).parse().unwrap_or(1500)));
            }
        }
        "hide" => win(&get(1)).iter().for_each(|w| w.window.set_visible(false)),
        "show" | "present" => win(&get(1)).iter().for_each(|w| w.window.present()),
        "minimize" => win(&get(1)).iter().for_each(|w| w.window.minimize()),
        "unminimize" => win(&get(1)).iter().for_each(|w| w.window.unminimize()),
        "status" => {
            if let Some(w) = win(&get(1)) {
                out(format!(
                    r#"{{"event":"status","name":"{}","realized":{},"mapped":{},"visible":{},"is_active":{},"has_frame_clock":{}}}"#,
                    get(1),
                    w.window.is_realized(),
                    w.window.is_mapped(),
                    w.window.is_visible(),
                    w.window.is_active(),
                    w.window.frame_clock().is_some()
                ));
            }
        }
        "quit" => app.quit(),
        _ => out(format!(r#"{{"event":"error","line":{:?}}}"#, line)),
    }
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id("dev.gtkmcp.S2").flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();
    let wins: Rc<RefCell<HashMap<String, Rc<Win>>>> = Rc::default();
    app.connect_activate(move |app| {
        let (tx, rx) = async_channel::unbounded::<String>();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if tx.send_blocking(line).is_err() {
                    break;
                }
            }
            let _ = tx.send_blocking("quit".into());
        });
        let app = app.clone();
        let wins = wins.clone();
        glib::spawn_future_local(async move {
            out(r#"{"event":"ready"}"#.into());
            while let Ok(line) = rx.recv().await {
                handle(&app, &wins, &line);
            }
        });
    });
    let hold = app.hold();
    let code = app.run_with_args::<&str>(&[]);
    drop(hold);
    code
}
