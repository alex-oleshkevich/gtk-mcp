//! FIXTURE_TRAY=1: a status/tray-style icon — a GtkImage with a
//! GtkGestureClick that increments a counter, with no tooltip or label and
//! its accessible role left at the GtkImage default ("img"). The user can
//! click it; assistive technology sees a static image (issue 28).

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::Cell;
use std::rc::Rc;

/// A second toplevel with the clickable icon.
pub fn spawn(app: &adw::Application) {
    let win = gtk::Window::builder().application(app).title("fixture tray").resizable(false).build();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_top(12);
    row.set_margin_bottom(12);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let clicks = Rc::new(Cell::new(0u32));
    let counter = gtk::Label::new(Some("tray clicks: 0"));
    counter.set_widget_name("tray-clicks");
    let icon = gtk::Image::from_icon_name("battery-good-symbolic");
    icon.set_widget_name("tray-icon");
    icon.set_pixel_size(22);
    let click = gtk::GestureClick::new();
    {
        let clicks = clicks.clone();
        let counter = counter.clone();
        click.connect_released(move |_, _, _, _| {
            clicks.set(clicks.get() + 1);
            counter.set_label(&format!("tray clicks: {}", clicks.get()));
        });
    }
    icon.add_controller(click);
    row.append(&icon);
    row.append(&counter);

    // GTK adds these labels' gestures lazily, not during instance init.
    let selectable = gtk::Label::new(Some("selectable text"));
    selectable.set_widget_name("tray-selectable-label");
    selectable.set_selectable(true);
    row.append(&selectable);
    let link = gtk::Label::new(None);
    link.set_widget_name("tray-link-label");
    link.set_markup("<a href=\"https://example.com\">link text</a>");
    row.append(&link);
    win.set_child(Some(&row));
    win.present();
}
