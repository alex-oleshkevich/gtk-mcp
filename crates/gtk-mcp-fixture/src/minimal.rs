//! Issue 31: no check buttons, switches, editable widgets or other recorder classes.

use gtk4 as gtk;
use gtk::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::Cell;

pub fn build_window(app: &adw::Application) -> gtk::ApplicationWindow {
    let label = super::named(gtk::Label::new(Some("clicks: 0")), "minimal-counter");
    let button = super::named(gtk::Button::with_label("Click"), "minimal-button");
    let clicks = Cell::new(0u32);
    let counter = label.clone();
    button.connect_clicked(move |button| {
        let count = clicks.get() + 1;
        clicks.set(count);
        counter.set_label(&format!("clicks: {count}"));
        if count == 1 {
            // Echo the first click outside the agent request, so the regression
            // can assert a recorded app emission without a virtual input device.
            let button = button.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(button) = button.upgrade() {
                    button.emit_clicked();
                }
            });
        }
    });
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&label);
    root.append(&button);
    gtk::ApplicationWindow::builder()
        .application(app)
        .title("gtk-mcp minimal fixture")
        .decorated(false)
        .default_width(320)
        .default_height(120)
        .child(&root)
        .build()
}
