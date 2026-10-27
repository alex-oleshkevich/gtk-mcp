//! Fixture-like GTK app that dumps its own accessible tree through public
//! `GtkAccessible` getters only.

use glib::translate::*;
use gtk4 as gtk;
use gtk::gio;
use libadwaita as adw;
use libadwaita::prelude::*;
use serde_json::{Value, json};
use std::time::Duration;

fn accessible_id(a: &gtk::Accessible) -> Option<String> {
    // gtk_accessible_get_accessible_id, GTK 4.22+: resolved at run time like the agent will.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"gtk_accessible_get_accessible_id".as_ptr()) };
    if p.is_null() {
        return None;
    }
    let f: unsafe extern "C" fn(*mut gtk::ffi::GtkAccessible) -> *mut libc::c_char = unsafe { std::mem::transmute(p) };
    unsafe { from_glib_full(f(a.to_glib_none().0)) }
}

fn role_nick(a: &gtk::Accessible) -> String {
    let v = a.accessible_role().to_value();
    glib::EnumValue::from_value(&v).map(|(_, e)| e.nick().to_string()).unwrap_or_default()
}

fn dump(a: &gtk::Accessible, depth: usize) -> Value {
    let mut children = Vec::new();
    let mut c = a.first_accessible_child();
    while let Some(ch) = c {
        c = ch.next_accessible_sibling();
        if depth < 64 {
            children.push(dump(&ch, depth + 1));
        }
    }
    let mut m = serde_json::Map::new();
    m.insert("gtk_type".into(), json!(a.type_().name()));
    m.insert("role".into(), json!(role_nick(a)));
    if let Some(w) = a.dynamic_cast_ref::<gtk::Widget>() {
        m.insert("widget_name".into(), json!(w.widget_name().to_string()));
        m.insert("visible".into(), json!(w.is_visible()));
        m.insert("mapped".into(), json!(w.is_mapped()));
        m.insert("sensitive".into(), json!(w.is_sensitive()));
        let mut wc = 0;
        let mut x = w.first_child();
        while let Some(n) = x {
            wc += 1;
            x = n.next_sibling();
        }
        m.insert("widget_children".into(), json!(wc));
        if let Some(t) = w.downcast_ref::<gtk::Window>() {
            m.insert("title".into(), json!(t.title().map(|s| s.to_string())));
        }
    }
    if let Some(id) = accessible_id(a) {
        m.insert("accessible_id".into(), json!(id));
    }
    m.insert("bounds".into(), json!(a.bounds()));
    m.insert("focusable".into(), json!(a.platform_state(gtk::AccessiblePlatformState::Focusable)));
    m.insert("focused".into(), json!(a.platform_state(gtk::AccessiblePlatformState::Focused)));
    m.insert("active".into(), json!(a.platform_state(gtk::AccessiblePlatformState::Active)));
    m.insert("has_parent".into(), json!(a.accessible_parent().is_some()));
    m.insert("children".into(), Value::Array(children));
    Value::Object(m)
}

fn dump_all() -> Value {
    let tops: Vec<Value> = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.dynamic_cast::<gtk::Accessible>().ok())
        .map(|a| dump(&a, 0))
        .collect();
    json!({"pid": std::process::id(), "toplevels": tops})
}

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

fn build(app: &adw::Application) -> (adw::ApplicationWindow, gtk::MenuButton) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    menu.append(Some("About"), Some("app.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    let mb = named(gtk::MenuButton::new(), "main-menu");
    mb.set_icon_name("open-menu-symbolic");
    mb.set_menu_model(Some(&menu));
    header.pack_end(&mb);
    root.append(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.append(&named(gtk::Label::new(Some("clicks: 0")), "counter"));
    let save = named(gtk::Button::with_label("Save"), "save");
    content.append(&save);
    let labelled = named(gtk::Button::with_label("raw"), "labelled-button");
    labelled.update_property(&[gtk::accessible::Property::Label("Explicit name")]);
    content.append(&labelled);
    content.append(&named(gtk::Button::from_icon_name("edit-delete-symbolic"), "icon-only"));
    let tipped = named(gtk::Button::from_icon_name("list-add-symbolic"), "icon-tooltip");
    tipped.set_tooltip_text(Some("Add item"));
    content.append(&tipped);
    let hidden = named(gtk::Button::with_label("Hidden"), "hidden-button");
    hidden.set_visible(false);
    content.append(&hidden);
    let insensitive = named(gtk::Button::with_label("Insensitive"), "insensitive-button");
    insensitive.set_sensitive(false);
    content.append(&insensitive);

    let label = gtk::Label::new(Some("Name:"));
    let entry = named(gtk::Entry::new(), "name-entry");
    entry.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
    let eb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    eb.append(&label);
    eb.append(&entry);
    content.append(&eb);
    content.append(&named(gtk::Entry::new(), "bare-entry"));
    let pw = named(gtk::PasswordEntry::new(), "password");
    pw.set_show_peek_icon(true);
    content.append(&pw);
    content.append(&named(gtk::Switch::new(), "switch"));
    content.append(&named(gtk::CheckButton::with_label("Remember"), "remember"));
    content.append(&named(gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0), "scale"));
    content.append(&named(gtk::SpinButton::with_range(0.0, 10.0, 1.0), "spin"));
    content.append(&named(gtk::DropDown::from_strings(&["Alpha", "Beta"]), "dropdown"));
    content.append(&named(gtk::LinkButton::with_label("https://example.org", "Link"), "link"));
    let exp = named(gtk::Expander::new(Some("Advanced")), "expander");
    exp.set_child(Some(&gtk::Entry::new()));
    content.append(&exp);
    let stack = named(gtk::Stack::new(), "stack");
    stack.add_titled(&gtk::Label::new(Some("A")), Some("a"), "Page A");
    stack.add_titled(&gtk::Label::new(Some("B")), Some("b"), "Page B");
    let sw = gtk::StackSwitcher::new();
    sw.set_stack(Some(&stack));
    content.append(&sw);
    content.append(&stack);
    let nb = named(gtk::Notebook::new(), "notebook");
    nb.append_page(&gtk::Label::new(Some("one")), Some(&gtk::Label::new(Some("Tab 1"))));
    nb.append_page(&gtk::Label::new(Some("two")), Some(&gtk::Label::new(Some("Tab 2"))));
    content.append(&nb);
    let img = named(gtk::Image::from_icon_name("dialog-information-symbolic"), "image-plain");
    content.append(&img);
    let img2 = named(gtk::Image::from_icon_name("dialog-warning-symbolic"), "image-described");
    img2.update_property(&[gtk::accessible::Property::Description("A warning sign")]);
    content.append(&img2);
    let tv = named(gtk::TextView::new(), "textview");
    tv.buffer().set_text("notes");
    let tvs = gtk::ScrolledWindow::new();
    tvs.set_min_content_height(50);
    tvs.set_child(Some(&tv));
    content.append(&tvs);

    let model = gtk::StringList::new(&[]);
    for i in 0..200 {
        model.append(&format!("Item {i}"));
    }
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, i| {
        i.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&gtk::Label::new(None)));
    });
    factory.connect_bind(|_, i| {
        let i = i.downcast_ref::<gtk::ListItem>().unwrap();
        let s = i.item().and_downcast::<gtk::StringObject>().unwrap();
        i.child().and_downcast::<gtk::Label>().unwrap().set_label(&s.string());
    });
    let lv = named(gtk::ListView::new(Some(gtk::SingleSelection::new(Some(model))), Some(factory)), "listview");
    let lvs = gtk::ScrolledWindow::new();
    lvs.set_min_content_height(80);
    lvs.set_child(Some(&lv));
    content.append(&lvs);

    // libadwaita rows (no buildable ids, like relm4 widgets).
    let group = adw::PreferencesGroup::new();
    group.set_title("Prefs");
    let er = adw::EntryRow::new();
    er.set_title("Entry row");
    group.add(&er);
    let sr = adw::SwitchRow::new();
    sr.set_title("Switch row");
    group.add(&sr);
    let ar = adw::ActionRow::new();
    ar.set_title("Action row");
    ar.set_subtitle("with subtitle");
    ar.add_suffix(&gtk::Button::from_icon_name("go-next-symbolic"));
    group.add(&ar);
    let cr = adw::ComboRow::new();
    cr.set_title("Combo");
    cr.set_model(Some(&gtk::StringList::new(&["x", "y"])));
    group.add(&cr);
    let pr = adw::PasswordEntryRow::new();
    pr.set_title("Token");
    group.add(&pr);
    content.append(&group);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_child(Some(&content));
    root.append(&scroller);

    let win = adw::ApplicationWindow::builder().application(app).title("S3 main").default_width(520).default_height(900).content(&root).build();
    (win, mb)
}

pub fn run(out: &str, stop: &str) {
    let app = adw::Application::builder().application_id("org.example.S3Spike").build();
    let (out, stop) = (out.to_string(), stop.to_string());
    app.connect_activate(move |app| {
        let (win, mb) = build(app);
        win.present();
        let second = gtk::Window::builder().title("S3 second").default_width(300).default_height(200).build();
        let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
        b.append(&gtk::Label::new(Some("second window")));
        b.append(&gtk::Button::with_label("Close"));
        second.set_child(Some(&b));
        second.present();
        let out = out.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            let closed = dump_all();
            mb.popup();
            let out2 = out.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                let open = dump_all();
                let v = json!({"closed": closed, "popover_open": open});
                std::fs::write(&out2, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
                eprintln!("s3 app: tree dumped to {out2}");
            });
        });
        let stop = stop.clone();
        let app1 = app.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            if std::path::Path::new(&stop).exists() {
                app1.quit();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        let app2 = app.clone();
        glib::timeout_add_local_once(Duration::from_secs(90), move || app2.quit());
    });
    app.run_with_args::<&str>(&[]);
}
