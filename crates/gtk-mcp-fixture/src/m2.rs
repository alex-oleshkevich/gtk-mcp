//! Fixture additions for the `actions` toolset (M2, AC-7): widget-scoped
//! action groups (one registered with the agent, one deliberately not), shortcut
//! controllers (managed, local, callback), a menu with sections and a submenu,
//! and a gesture controller. Widget names are the `automation_id` selectors
//! used by `tests/fixture_m2.rs`.

use gtk::{gio, glib};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::Cell;
use std::rc::Rc;

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

pub fn extras(root: &gtk::Box, win: &adw::ApplicationWindow) {
    let m2 = named(gtk::Box::new(gtk::Orientation::Vertical, 4), "m2-box");
    let status = named(gtk::Label::new(Some("m2: idle")), "m2-status");
    m2.append(&status);

    // Group "panel": inserted with `insert_action_group` and NOT registered.
    let pings = Rc::new(Cell::new(0u32));
    let unreg = named(gtk::Box::new(gtk::Orientation::Horizontal, 0), "m2-unregistered-panel");
    let panel = gio::SimpleActionGroup::new();
    let ping = gio::SimpleAction::new("ping", None);
    {
        let (pings, status) = (pings.clone(), status.clone());
        ping.connect_activate(move |_, _| {
            pings.set(pings.get() + 1);
            status.set_label(&format!("m2: pings {}", pings.get()));
        });
    }
    panel.add_action(&ping);
    let echo = gio::SimpleAction::new("echo", Some(glib::VariantTy::STRING));
    {
        let status = status.clone();
        echo.connect_activate(move |_, p| {
            status.set_label(&format!("m2: echo {}", p.and_then(|p| p.get::<String>()).unwrap_or_default()));
        });
    }
    panel.add_action(&echo);
    unreg.insert_action_group("panel", Some(&panel));
    m2.append(&unreg);

    // Group "reg": inserted AND registered with the agent.
    let reg_box = named(gtk::Box::new(gtk::Orientation::Horizontal, 0), "m2-registered-panel");
    let reg = gio::SimpleActionGroup::new();
    let toggle = gio::SimpleAction::new_stateful("toggle", None, &false.to_variant());
    reg.add_action(&toggle); // boolean stateful without handler: GIO toggles on activate
    let level = gio::SimpleAction::new_stateful("level", Some(glib::VariantTy::INT32), &2i32.to_variant());
    level.set_state_hint(Some(&(0i32, 10i32).to_variant()));
    level.connect_change_state(|a, v| {
        if let Some(v) = v {
            a.set_state(v);
        }
    });
    level.connect_activate(|a, p| {
        if let Some(p) = p {
            a.change_state(p);
        }
    });
    reg.add_action(&level);
    let record = gio::SimpleAction::new("record", Some(glib::VariantTy::new("(si)").unwrap()));
    {
        let status = status.clone();
        record.connect_activate(move |_, p| {
            if let Some((name, n)) = p.and_then(|p| p.get::<(String, i32)>()) {
                status.set_label(&format!("m2: record {name}={n}"));
            }
        });
    }
    reg.add_action(&record);
    let off = gio::SimpleAction::new("off", None);
    off.set_enabled(false);
    reg.add_action(&off);
    reg_box.insert_action_group("reg", Some(&reg));
    gtk_mcp_agent::register_action_group(&reg_box, "reg", &reg);
    m2.append(&reg_box);

    // Shortcuts: <Control>s -> win.save (managed, on the window), a callback
    // shortcut, and a local controller on an entry.
    let callbacks = Rc::new(Cell::new(0u32));
    let cb_label = named(gtk::Label::new(Some("m2 callbacks: 0")), "m2-callback-count");
    m2.append(&cb_label);
    let managed = gtk::ShortcutController::new();
    managed.set_scope(gtk::ShortcutScope::Managed);
    managed.set_name(Some("m2-window-shortcuts"));
    managed.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>s"),
        Some(gtk::NamedAction::new("win.save")),
    ));
    {
        let (callbacks, cb_label) = (callbacks.clone(), cb_label.clone());
        managed.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control><Shift>k"),
            Some(gtk::CallbackAction::new(move |_, _| {
                callbacks.set(callbacks.get() + 1);
                cb_label.set_label(&format!("m2 callbacks: {}", callbacks.get()));
                glib::Propagation::Stop
            })),
        ));
    }
    managed.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>l|F6"),
        Some(gtk::NamedAction::new("reg.toggle")), // not reachable from the window: reports unhandled
    ));
    win.add_controller(managed);

    let entry = named(gtk::Entry::new(), "m2-shortcut-entry");
    let local = gtk::ShortcutController::new();
    local.set_name(Some("m2-entry-shortcuts"));
    {
        let (callbacks, cb_label) = (callbacks.clone(), cb_label.clone());
        local.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Alt>e"),
            Some(gtk::CallbackAction::new(move |_, _| {
                callbacks.set(callbacks.get() + 10);
                cb_label.set_label(&format!("m2 callbacks: {}", callbacks.get()));
                glib::Propagation::Stop
            })),
        ));
    }
    entry.add_controller(local);
    m2.append(&entry);

    // A registered group on a password entry: its state is secret (plan 4.4).
    let vault_entry = named(gtk::PasswordEntry::new(), "m2-secret-entry");
    let vault = gio::SimpleActionGroup::new();
    let token = gio::SimpleAction::new_stateful("token", Some(glib::VariantTy::STRING), &"hunter2-SECRET-0xC0FFEE".to_variant());
    token.connect_change_state(|a, v| {
        if let Some(v) = v {
            a.set_state(v);
        }
    });
    vault.add_action(&token);
    vault_entry.insert_action_group("vault", Some(&vault));
    gtk_mcp_agent::register_action_group(&vault_entry, "vault", &vault);
    m2.append(&vault_entry);

    // An application accelerator for a window action.
    if let Some(app) = win.application() {
        app.set_accels_for_action("win.theme::dark", &["<Control><Alt>d"]);
    }

    // Menu with sections and a submenu.
    let menu = gio::Menu::new();
    let edit = gio::Menu::new();
    edit.append(Some("Save"), Some("win.save"));
    let item = gio::MenuItem::new(Some("Limit 5"), None);
    item.set_action_and_target_value(Some("win.limit"), Some(&5i32.to_variant()));
    item.set_attribute_value("icon", Some(&"document-save-symbolic".to_variant()));
    edit.append_item(&item);
    menu.append_section(Some("Edit"), &edit);
    let theme = gio::Menu::new();
    theme.append(Some("Dark"), Some("win.theme::dark"));
    theme.append(Some("Light"), Some("win.theme::light"));
    let more = gio::Menu::new();
    more.append_submenu(Some("Theme"), &theme);
    more.append(Some("Toggle (reg)"), Some("reg.toggle"));
    more.append(Some("Missing"), Some("win.does-not-exist"));
    menu.append_submenu(Some("More"), &more);
    let menu_button = named(gtk::MenuButton::new(), "m2-menu-button");
    menu_button.set_label("M2 menu");
    menu_button.set_menu_model(Some(&menu));
    m2.append(&menu_button);
    // An actionable button bound to a stateful radio-style action.
    let bound = named(gtk::Button::with_label("Dark theme"), "m2-theme-button");
    bound.set_action_name(Some("win.theme"));
    bound.set_action_target_value(Some(&"dark".to_variant()));
    m2.append(&bound);

    // A gesture controller on a label.
    let clickable = named(gtk::Label::new(Some("right-click me")), "m2-clickable");
    let click = gtk::GestureClick::new();
    click.set_button(3);
    click.set_exclusive(true);
    click.set_touch_only(false);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.set_name(Some("m2-click"));
    clickable.add_controller(click);
    m2.append(&clickable);

    root.append(&m2);
}
