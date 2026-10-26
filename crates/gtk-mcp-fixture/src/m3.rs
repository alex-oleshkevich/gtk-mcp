//! Widgets for the M3 layout-style acceptance tests (AC-10, AC-12): layout
//! audit cases and an element styled by a known class. All sit in one section
//! named `m3-section`, appended to the window's content.

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

fn find_named(w: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if w.widget_name() == name {
        return Some(w.clone());
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if let Some(f) = find_named(&child, name) {
            return Some(f);
        }
        c = child.next_sibling();
    }
    None
}

pub fn extras(root: &gtk::Box) {
    // `long-label` (AC-12) has `max-width-chars=12` but fills the wide window,
    // so its text fits. Start alignment gives it exactly its 12 characters:
    // the ellipsis becomes real.
    if let Some(long) = find_named(root.upcast_ref(), "long-label") {
        long.set_halign(gtk::Align::Start);
    }
    // The "known class": the application styles `.m3-accent` blue; tests
    // restyle it through apply_css (user priority wins) and compare pixels.
    if let Some(display) = gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        #[allow(deprecated)]
        provider.load_from_data(".m3-accent { background-color: #2a7fff; color: #ffffff; padding: 4px; }");
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    let section = named(gtk::Box::new(gtk::Orientation::Vertical, 4), "m3-section");

    let styled = named(gtk::Label::new(Some("Styled by class")), "m3-styled");
    styled.add_css_class("m3-accent");
    styled.set_halign(gtk::Align::Start);
    styled.set_size_request(160, 36);
    section.append(&styled);

    // A label that fits.
    let fits = named(gtk::Label::new(Some("Fits")), "m3-fits");
    fits.set_halign(gtk::Align::Start);
    section.append(&fits);

    // An ellipsized label that cannot grow beyond 10 characters.
    let ell = named(gtk::Label::new(Some("This text is much longer than the width it gets")), "m3-ellipsized");
    ell.set_ellipsize(gtk::pango::EllipsizeMode::End);
    ell.set_max_width_chars(10);
    ell.set_halign(gtk::Align::Start);
    section.append(&ell);

    // A wrapping label: its height depends on the width it is given.
    let wrapping = named(
        gtk::Label::new(Some(
            "A wrapping label whose height depends on the width it is given: more width, fewer lines, less height.",
        )),
        "m3-wrapping",
    );
    wrapping.set_wrap(true);
    wrapping.set_xalign(0.0);
    section.append(&wrapping);

    // GtkInscription with an ellipsis: truncation cannot be detected (reported unsupported).
    let inscription = named(gtk::Inscription::new(Some("An inscription that is ellipsized")), "m3-inscription");
    inscription.set_text_overflow(gtk::InscriptionOverflow::EllipsizeEnd);
    inscription.set_min_chars(10);
    inscription.set_halign(gtk::Align::Start);
    section.append(&inscription);

    // A visible widget that has no size.
    let zero = named(gtk::Box::new(gtk::Orientation::Horizontal, 0), "m3-zero");
    section.append(&zero);

    // Overlapping siblings: a GtkOverlay whose child covers part of the base.
    let overlay = named(gtk::Overlay::new(), "m3-overlay");
    let base = named(gtk::Label::new(Some("Overlay base")), "m3-overlay-base");
    base.set_size_request(220, 40);
    overlay.set_child(Some(&base));
    let top = named(gtk::Button::with_label("Over"), "m3-overlay-top");
    top.set_halign(gtk::Align::Start);
    top.set_valign(gtk::Align::Start);
    overlay.add_overlay(&top);
    section.append(&overlay);

    // A child clipped outside its parent: a GtkOverlay measures only its main
    // child (an 80x30 spacer) but gives an overlay child at least its own
    // minimum width (200px); the overlay clips what does not fit.
    let clip_parent = named(gtk::Overlay::new(), "m3-clip-parent");
    clip_parent.set_overflow(gtk::Overflow::Hidden);
    clip_parent.set_halign(gtk::Align::Start);
    let spacer = named(gtk::Box::new(gtk::Orientation::Horizontal, 0), "m3-clip-spacer");
    spacer.set_size_request(80, 30);
    clip_parent.set_child(Some(&spacer));
    let clip_child = named(gtk::Button::with_label("clip"), "m3-clip-child");
    clip_child.set_size_request(200, 24);
    clip_child.set_halign(gtk::Align::Start);
    clip_child.set_valign(gtk::Align::Start);
    clip_parent.add_overlay(&clip_child);
    section.append(&clip_parent);

    // Icon names the theme cannot resolve.
    let bad_icon = named(gtk::Image::from_icon_name("gtk-mcp-no-such-icon"), "m3-bad-icon");
    bad_icon.set_halign(gtk::Align::Start);
    section.append(&bad_icon);
    section.append(&named(gtk::Button::from_icon_name("gtk-mcp-no-such-button-icon"), "m3-bad-icon-button"));

    root.append(&section);
}
