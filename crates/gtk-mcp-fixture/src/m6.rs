//! Widgets for the M6 accessibility tests (AC-13): controls with and without
//! accessible names, an unlabeled image, a tiny target and an unfocusable
//! button, all in one section named `a11y-section`.

use gtk4 as gtk;
use gtk4::prelude::*;

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

pub fn extras(root: &gtk::Box) {
    let section = named(gtk::Box::new(gtk::Orientation::Vertical, 4), "a11y-section");

    // Named by its label.
    section.append(&named(gtk::Button::with_label("Accessible save"), "a11y-labeled"));

    // Icon only: no label, no tooltip, no accessible label.
    section.append(&named(gtk::Button::from_icon_name("edit-delete-symbolic"), "a11y-icon-unlabeled"));

    // Icon only, labelled through the accessible API.
    let labelled = named(gtk::Button::from_icon_name("list-add-symbolic"), "a11y-icon-labelled");
    labelled.update_property(&[gtk::accessible::Property::Label("Add item")]);
    section.append(&labelled);

    // Entries: one without any name, one with an accessible label.
    section.append(&named(gtk::Entry::new(), "a11y-entry-unlabeled"));
    let entry = named(gtk::Entry::new(), "a11y-entry-labelled");
    entry.update_property(&[gtk::accessible::Property::Label("Search term")]);
    section.append(&entry);

    // A standalone image without text alternative.
    section.append(&named(gtk::Image::from_icon_name("dialog-information-symbolic"), "a11y-image"));

    // Below the minimum target size.
    if let Some(display) = gtk::gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        #[allow(deprecated)]
        provider.load_from_data(".a11y-tiny { min-width: 10px; min-height: 10px; padding: 0; }");
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    let tiny = named(gtk::Button::with_label(""), "a11y-tiny");
    tiny.add_css_class("a11y-tiny");
    tiny.set_halign(gtk::Align::Start);
    tiny.set_valign(gtk::Align::Start);
    section.append(&tiny);

    // A control the keyboard cannot reach.
    let skipped = named(gtk::Button::with_label("Skip me"), "a11y-unfocusable");
    skipped.set_focusable(false);
    skipped.set_can_focus(false);
    section.append(&skipped);

    // Tab order inside the section: three labelled buttons.
    let tabs = named(gtk::Box::new(gtk::Orientation::Horizontal, 4), "a11y-tabs");
    for (i, n) in ["First", "Second", "Third"].iter().enumerate() {
        tabs.append(&named(gtk::Button::with_label(n), &format!("a11y-tab-{}", i + 1)));
    }
    section.append(&tabs);

    root.append(&section);
}
