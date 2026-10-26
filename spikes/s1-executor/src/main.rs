//! S1: does a `spawn_future_local` executor fed by an `async_channel` from a
//! plain thread deliver messages (a) queued before `RelmApp::run`, (b) during
//! the main loop, (c) after run() while the host iterates the default context?
//! The "socket thread" never touches GTK: it only sends `(label, Instant)`.
use relm4::gtk::{self, glib, prelude::*};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct App;
#[relm4::component]
impl SimpleComponent for App {
    type Init = ();
    type Input = ();
    type Output = ();
    view! { gtk::Window { set_default_size: (200, 100), gtk::Label { set_label: "s1" } } }
    fn init(_: (), root: Self::Root, _s: ComponentSender<Self>) -> ComponentParts<Self> {
        let w = view_output!();
        ComponentParts { model: App, widgets: w }
    }
}

fn main() {
    let t0 = Instant::now();
    let main_tid = std::thread::current().id();
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let (tx, rx) = async_channel::unbounded::<(&'static str, Instant)>();
    let (run_returned_tx, run_returned_rx) = std::sync::mpsc::channel::<()>();

    let app = RelmApp::new("dev.gtkmcp.s1").visible_on_activate(false);
    let gtk_app = relm4::main_application();

    // attach(): executor registered before run; handler records its thread.
    let l = log.clone();
    let quit_app = gtk_app.clone();
    glib::spawn_future_local(async move {
        while let Ok((label, sent)) = rx.recv().await {
            let on_main = std::thread::current().id() == main_tid;
            l.lock().unwrap().push(format!(
                "{label}: on_main={on_main} latency={:?} t={:?}",
                sent.elapsed(),
                t0.elapsed()
            ));
            if label == "during" {
                quit_app.quit();
            }
        }
    });

    // "socket thread": plain std thread, no GTK.
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        tx2.send_blocking(("before-run", Instant::now())).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        tx2.send_blocking(("during", Instant::now())).unwrap();
        run_returned_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        tx2.send_blocking(("post-run", Instant::now())).unwrap();
    });

    app.with_args(vec![]).run::<App>(());
    // relm4's run() ends with `shutdown_all(); MainContext::iteration(true)`.
    log.lock().unwrap().push(format!("run() returned t={:?}", t0.elapsed()));
    run_returned_tx.send(()).ok();
    std::thread::sleep(Duration::from_millis(600));
    let mut n = 0;
    while glib::MainContext::default().iteration(false) {
        n += 1;
    }
    log.lock().unwrap().push(format!("explicit post-run iterations dispatched={n}"));
    for line in log.lock().unwrap().iter() {
        println!("{line}");
    }
}
