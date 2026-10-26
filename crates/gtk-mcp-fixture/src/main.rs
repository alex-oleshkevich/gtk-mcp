//! Test application for the gtk-mcp agent (plan 6, section 9: every AC runs
//! against this binary). Widgets carry explicit widget names, which are the
//! `automation_id` selectors used by the tests.

use gtk::{gio, glib};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    /// Strong reference that keeps a removed widget alive until "finalize-widget".
    static VICTIM: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
}

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

fn block_main_loop(d: Duration) {
    // Deliberately blocks the GTK main loop (debug hook for AC-8).
    std::thread::sleep(d);
}

fn build_window(app: &adw::Application) -> adw::ApplicationWindow {
    let clicks = Rc::new(Cell::new(0u32));
    let counter = named(gtk::Label::new(Some("clicks: 0")), "click-counter");
    let bump = {
        let clicks = clicks.clone();
        let counter = counter.clone();
        move || {
            clicks.set(clicks.get() + 1);
            counter.set_label(&format!("clicks: {}", clicks.get()));
        }
    };

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.append(&counter);
    // Reports GTK_MCP* variables still visible to std::env after attach (must be none).
    let leaked: Vec<String> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("GTK_MCP")).collect();
    let env_report = named(
        gtk::Label::new(Some(&format!("gtk-mcp-env: {}", if leaked.is_empty() { "none".to_string() } else { leaked.join(",") }))),
        "env-report",
    );
    root.append(&env_report);

    // Collapsed expander with an entry child (detached content, AC-2/AC-4/AC-5).
    let expander = named(gtk::Expander::new(Some("Advanced options")), "advanced-expander");
    let server = named(gtk::Entry::new(), "server-address");
    server.set_text("localhost");
    expander.set_child(Some(&server));
    root.append(&expander);

    // Two buttons labelled "Save" (AC-3).
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for name in ["save-primary", "save-secondary"] {
        let b = named(gtk::Button::with_label("Save"), name);
        let bump = bump.clone();
        b.connect_clicked(move |_| bump());
        row.append(&b);
    }
    root.append(&row);

    // Secrets (AC-6).
    let password = named(gtk::PasswordEntry::new(), "password");
    password.set_show_peek_icon(true);
    root.append(&password);
    let prow = named(adw::PasswordEntryRow::new(), "password-row");
    prow.set_title("Token");
    let plist = gtk::ListBox::new();
    plist.add_css_class("boxed-list");
    plist.append(&prow);
    root.append(&plist);

    // Form: entry whose content makes "submit" sensitive (wait_for acceptance).
    let name_entry = named(gtk::Entry::new(), "name-entry");
    name_entry.set_placeholder_text(Some("Your name"));
    let submit = named(gtk::Button::with_label("Submit"), "submit");
    submit.set_sensitive(false);
    {
        let submit = submit.clone();
        name_entry.connect_changed(move |e| submit.set_sensitive(!e.text().is_empty()));
    }
    {
        let bump = bump.clone();
        submit.connect_clicked(move |_| bump());
    }
    root.append(&name_entry);
    root.append(&submit);

    // Switch, check, scale, dropdown.
    root.append(&named(gtk::Switch::new(), "feature-switch"));
    root.append(&named(gtk::CheckButton::with_label("Remember me"), "remember"));
    let scale = named(gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0), "volume");
    scale.set_value(25.0);
    root.append(&scale);
    let choices = named(gtk::DropDown::from_strings(&["Alpha", "Beta", "Gamma"]), "choices");
    root.append(&choices);

    // Stack with two pages.
    let stack = named(gtk::Stack::new(), "stack");
    stack.add_titled(&gtk::Label::new(Some("Page A content")), Some("page-a"), "Page A");
    stack.add_titled(&gtk::Label::new(Some("Page B content")), Some("page-b"), "Page B");
    root.append(&stack);

    // Text view.
    let notes = named(gtk::TextView::new(), "notes");
    notes.buffer().set_text("hello notes");
    let notes_scroll = gtk::ScrolledWindow::new();
    notes_scroll.set_min_content_height(60);
    notes_scroll.set_child(Some(&notes));
    root.append(&notes_scroll);

    // Ellipsized label (AC-12).
    let long = named(
        gtk::Label::new(Some("This label text is far too long to fit into the narrow space it was given")),
        "long-label",
    );
    long.set_ellipsize(gtk::pango::EllipsizeMode::End);
    long.set_max_width_chars(12);
    root.append(&long);

    // List view with a model larger than its viewport (AC-2 acceptance).
    let model = gtk::StringList::new(&[]);
    for i in 0..500 {
        model.append(&format!("Item {i}"));
    }
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(&gtk::Label::new(None)));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let s = item.item().and_downcast::<gtk::StringObject>().unwrap();
        item.child().and_downcast::<gtk::Label>().unwrap().set_label(&s.string());
    });
    let selection = gtk::SingleSelection::new(Some(model));
    let list = named(gtk::ListView::new(Some(selection), Some(factory)), "items-list");
    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_min_content_height(100);
    list_scroll.set_max_content_height(100);
    list_scroll.set_child(Some(&list));
    root.append(&list_scroll);

    // Menu button with a popover menu (closed popover appears once, in place).
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("Save"), Some("win.save"));
    section.append(Some("Theme: dark"), Some("win.theme::dark"));
    section.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &section);
    let menu_button = named(gtk::MenuButton::new(), "menu-button");
    menu_button.set_label("Menu");
    menu_button.set_menu_model(Some(&menu));
    root.append(&menu_button);

    // Debug hooks (AC-8, AC-3): triggered by tests through `interact activate`.
    let block = named(gtk::Button::with_label("Block 5s"), "block-5s");
    block.connect_clicked(|_| block_main_loop(Duration::from_secs(5)));
    let slow = named(gtk::Button::with_label("Slow action"), "slow-op");
    {
        let bump = bump.clone();
        slow.connect_clicked(move |_| {
            block_main_loop(Duration::from_secs(2));
            bump();
        });
    }
    let hooks = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    hooks.append(&block);
    hooks.append(&slow);
    let victim_box = named(gtk::Box::new(gtk::Orientation::Horizontal, 0), "victim-box");
    let victim = named(gtk::Label::new(Some("victim")), "victim");
    victim_box.append(&victim);
    let remove = named(gtk::Button::with_label("Remove widget"), "remove-widget");
    {
        let victim_box = victim_box.clone();
        remove.connect_clicked(move |_| {
            if let Some(v) = victim_box.first_child().and_downcast::<gtk::Label>() {
                victim_box.remove(&v);
                VICTIM.with(|c| *c.borrow_mut() = Some(v)); // alive but detached
            }
        });
    }
    let finalize = named(gtk::Button::with_label("Finalize widget"), "finalize-widget");
    finalize.connect_clicked(|_| VICTIM.with(|c| *c.borrow_mut() = None));
    hooks.append(&remove);
    hooks.append(&finalize);
    root.append(&hooks);
    root.append(&victim_box);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);

    let win = adw::ApplicationWindow::builder().application(app).title("gtk-mcp fixture").default_width(480).default_height(720).content(&scroll).build();

    // Actions (AC-7): win.save, stateful win.theme (string), and win.limit (int
    // state that rejects values above 10).
    let save = gio::SimpleAction::new("save", None);
    {
        let bump = bump.clone();
        save.connect_activate(move |_, _| bump());
    }
    win.add_action(&save);
    let theme = gio::SimpleAction::new_stateful("theme", Some(glib::VariantTy::STRING), &"light".to_variant());
    theme.connect_activate(|a, p| {
        if let Some(p) = p {
            a.change_state(p);
        }
    });
    theme.connect_change_state(|a, v| {
        if let Some(v) = v {
            a.set_state(v);
        }
    });
    win.add_action(&theme);
    let limit = gio::SimpleAction::new_stateful("limit", Some(glib::VariantTy::INT32), &0i32.to_variant());
    limit.connect_change_state(|a, v| {
        if let Some(v) = v {
            if v.get::<i32>().is_some_and(|n| n <= 10) {
                a.set_state(v); // out-of-range requests are rejected by the app
            }
        }
    });
    win.add_action(&limit);
    win
}

fn main() -> glib::ExitCode {
    // FIXTURE_UNIQUE=1 keeps GApplication uniqueness, so a second instance is a
    // remote one (it never emits `startup`, hence never binds a socket).
    let flags = if std::env::var_os("FIXTURE_UNIQUE").is_some() { gio::ApplicationFlags::empty() } else { gio::ApplicationFlags::NON_UNIQUE };
    let app = adw::Application::builder().application_id("dev.gtkmcp.Fixture").flags(flags).build();
    gtk_mcp_agent::attach(&app);
    let quit = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit);
    app.connect_activate(|app| {
        let win = build_window(app);
        win.present();
    });
    app.run()
}
