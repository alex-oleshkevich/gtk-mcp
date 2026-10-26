//! M2 acceptance against the real fixture: AC-7 (actions discoverable and
//! invocable) plus menus, shortcuts and controllers. Every call is bounded
//! (broker deadlines + the harness watchdog).

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::ErrorCode;
use serde_json::{Value, json};
use std::sync::Arc;

const SECRET: &str = "hunter2-SECRET-0xC0FFEE";

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

fn broker(fx: &Fixture, allow_mutations: bool) -> Arc<Broker> {
    let policy = Policy { allow_mutations, toolsets: vec!["core".into(), "actions".into()], ..Policy::default() };
    Broker::new(policy, Config::with_socket_dir(fx.dir.path())).unwrap()
}

fn rw(fx: &Fixture) -> Client {
    Client { broker: broker(fx, true) }
}

async fn counter(c: &Client) -> String {
    c.json("inspect", json!({"target": by_id("click-counter")})).await["text"].as_str().unwrap().to_string()
}

async fn status(c: &Client) -> String {
    c.json("inspect", json!({"target": by_id("m2-status")})).await["text"].as_str().unwrap().to_string()
}

fn action<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("action {name} not listed in {list}"))
}

fn has_action(list: &Value, name: &str) -> bool {
    list["actions"].as_array().unwrap().iter().any(|a| a["name"] == name)
}

// ---- AC-7 ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ac7_win_save_is_listed_and_activation_runs_the_handler_once() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    let list = c.json("list_actions", json!({"scope": "window"})).await;
    let save = action(&list, "win.save");
    assert_eq!(save["enabled"], true);
    assert_eq!(save["parameter_type"], Value::Null);
    assert_eq!(save["state_type"], Value::Null);
    assert_eq!(save["source"]["kind"], "window");
    let theme = action(&list, "win.theme");
    assert_eq!(theme["parameter_type"], "s");
    assert_eq!(theme["state_type"], "s");
    assert_eq!(theme["state"], "light");
    assert_eq!(action(&list, "win.limit")["state"], 0);
    // GTK enumerates window-scope class actions as well; their enabled state is unknown.
    let class_entry = list["actions"].as_array().unwrap().iter().find(|a| a["source"]["kind"] == "widget_class");
    if let Some(e) = class_entry {
        assert_eq!(e["enabled"], Value::Null);
    }

    assert_eq!(counter(&c).await, "clicks: 0");
    let r = c.json("activate_action", json!({"scope": "window", "name": "win.save"})).await;
    assert_eq!(r["activated"], true);
    assert_eq!(r["action"], "win.save");
    assert_eq!(r["observed"]["enabled"], true);
    assert_eq!(counter(&c).await, "clicks: 1", "the handler ran exactly once");

    // the scope may be inferred from the prefix
    c.json("activate_action", json!({"name": "win.save"})).await;
    assert_eq!(counter(&c).await, "clicks: 2");

    // the application scope is complete and has app.quit (never activated here)
    let app = c.json("list_actions", json!({"scope": "app"})).await;
    assert_eq!(app["complete"], true);
    assert!(has_action(&app, "app.quit"));
    assert_eq!(action(&app, "app.quit")["source"]["kind"], "app");

    // observe attaches a post-action snapshot of the window
    let r = c.json("activate_action", json!({"name": "win.save", "observe": {"snapshot_depth": 1}})).await;
    assert!(r["observation"]["snapshot"].is_object() || r["observation"]["snapshot"].is_string(), "{r}");
    assert_eq!(counter(&c).await, "clicks: 3");
}

#[tokio::test(flavor = "multi_thread")]
async fn ac7_change_action_state_returns_observed_state_including_rejection() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    let before = c.json("inspect_action", json!({"name": "win.limit"})).await;
    assert_eq!(before["state_type"], "i");
    assert_eq!(before["state"], 0);
    assert_eq!(before["parameter_type"], "i");
    assert_eq!(before["scope"]["kind"], "window");

    let ok = c.json("change_action_state", json!({"name": "win.limit", "value": 7})).await;
    assert_eq!(ok["accepted"], true);
    assert_eq!(ok["observed"]["state"], 7);
    assert_eq!(ok["requested"], 7);
    assert_eq!(ok["state_before"], 0);

    // the fixture rejects values above 10: the observed state stays 7
    let rej = c.json("change_action_state", json!({"name": "win.limit", "value": 50})).await;
    assert_eq!(rej["accepted"], false);
    assert_eq!(rej["observed"]["state"], 7);
    assert_eq!(rej["requested"], 50);
    assert_eq!(c.json("inspect_action", json!({"name": "win.limit"})).await["state"], 7);

    // stateful string action through both tools
    let r = c.json("change_action_state", json!({"name": "win.theme", "value": "dark"})).await;
    assert_eq!(r["accepted"], true);
    assert_eq!(r["observed"]["state"], "dark");
    let r = c.json("activate_action", json!({"name": "win.theme", "parameter": "light"})).await;
    assert_eq!(r["state_before"], "dark");
    assert_eq!(r["observed"]["state"], "light");

    // invalid values never reach the app
    let e = c.err("change_action_state", json!({"name": "win.limit", "value": "seven"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    assert!(!e.message.contains("seven"));
    let e = c.err("change_action_state", json!({"name": "win.limit", "value": 4_000_000_000u64})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("change_action_state", json!({"name": "win.save", "value": true})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let e = c.err("activate_action", json!({"name": "win.theme", "parameter": 5})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("activate_action", json!({"name": "win.theme"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("activate_action", json!({"name": "win.save", "parameter": "x"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("activate_action", json!({"name": "win.nope"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("inspect_action", json!({"name": "win.nope"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("activate_action", json!({"scope": "app", "name": "win.save"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("activate_action", json!({"name": "save"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert_eq!(counter(&c).await, "clicks: 0");
}

/// The first late result recorded at or after event cursor `after` (waits at most 10 s).
async fn late_result(c: &Client, after: u64) -> Value {
    let session = c.broker.sessions().remove(0);
    let t = std::time::Instant::now();
    loop {
        let evs = session.events_since(after);
        if let Some(l) = evs.entries.iter().find(|e| e.value["kind"] == "late_result") {
            return l.value.clone();
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "late result never recorded");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_that_outlive_the_deadline_are_reported_as_late_results() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let policy = Policy { allow_mutations: true, toolsets: vec!["core".into(), "actions".into()], ..Policy::default() };
    let mut cfg = Config::with_socket_dir(fx.dir.path());
    cfg.default_deadline_ms = 1000;
    let c = Client { broker: Broker::new(policy, cfg).unwrap() };
    c.json("list_apps", json!({})).await;

    // win.slow blocks the main loop for 2 s inside its handlers.
    let e = c.err("activate_action", json!({"name": "win.slow"})).await;
    assert_eq!(e.code, ErrorCode::Timeout, "{e:?}");
    assert_eq!(e.data.as_ref().unwrap()["started"], true);
    let late = late_result(&c, 0).await;
    assert_eq!(late["result"]["activated"], true, "{late}");
    assert_eq!(late["result"]["action"], "win.slow");

    let cursor = c.broker.sessions().remove(0).events_since(0).next_cursor;
    let e = c.err("change_action_state", json!({"name": "win.slow", "value": 5})).await;
    assert_eq!(e.code, ErrorCode::Timeout, "{e:?}");
    let late = late_result(&c, cursor).await;
    assert_eq!((&late["result"]["accepted"], &late["result"]["observed"]["state"]), (&json!(true), &json!(5)), "{late}");
    assert_eq!(counter(&rw(&fx)).await, "clicks: 1", "the activation ran exactly once and was never retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_widget_group_is_listed_typed_and_activated() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let scope = by_id("m2-registered-panel");

    let list = c.json("list_actions", json!({"scope": scope, "prefix": "reg"})).await;
    assert_eq!(list["complete"], false);
    assert!(list["incomplete_reason"].as_str().unwrap().contains("register_action_group"));
    assert_eq!(list["scope"]["kind"], "widget");
    for n in ["reg.toggle", "reg.level", "reg.record", "reg.off"] {
        assert_eq!(action(&list, n)["source"]["kind"], "widget", "{n}");
        assert_eq!(action(&list, n)["source"]["registered"], true);
    }
    assert_eq!(list["total"], 4);
    let level = action(&list, "reg.level");
    assert_eq!(level["state_hint"], json!({"range": [0, 10]}));
    assert_eq!(level["state"], 2);
    assert_eq!(action(&list, "reg.record")["parameter_type"], "(si)");
    assert_eq!(action(&list, "reg.off")["enabled"], false);

    // everything reachable from the widget: the window and the application too
    let all = c.json("list_actions", json!({"scope": scope})).await;
    assert!(has_action(&all, "win.save") && has_action(&all, "app.quit") && has_action(&all, "reg.toggle"));
    assert_eq!(action(&all, "win.save")["source"]["kind"], "window");
    // the window scope does not list widget groups
    let win = c.json("list_actions", json!({"scope": "window"})).await;
    assert!(!has_action(&win, "reg.toggle"));

    // boolean toggle: GIO flips the state on activation
    let r = c.json("activate_action", json!({"scope": scope, "name": "reg.toggle"})).await;
    assert_eq!(r["observed"]["state"], true);
    assert_eq!(r["state_before"], false);
    // structured parameter converted by the action's type (si)
    c.json("activate_action", json!({"scope": scope, "name": "reg.record", "parameter": ["alpha", 3]})).await;
    assert_eq!(status(&c).await, "m2: record alpha=3");
    // GVariant text is accepted for non-string types
    c.json("activate_action", json!({"scope": scope, "name": "reg.record", "parameter": "('beta', 4)"})).await;
    assert_eq!(status(&c).await, "m2: record beta=4");
    let e = c.err("activate_action", json!({"scope": scope, "name": "reg.record", "parameter": ["alpha"]})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("activate_action", json!({"scope": scope, "name": "reg.record", "parameter": ["alpha", "x"]})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    // disabled actions do not run
    let e = c.err("activate_action", json!({"scope": scope, "name": "reg.off"})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    // stateful change through the registered group (hint is advisory only)
    let r = c.json("change_action_state", json!({"scope": scope, "name": "reg.level", "value": 9})).await;
    assert_eq!(r["accepted"], true);
    assert_eq!(r["observed"]["state"], 9);
    // widget groups need a widget scope
    let e = c.err("activate_action", json!({"scope": "app", "name": "reg.toggle"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("activate_action", json!({"name": "reg.toggle"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
}

#[tokio::test(flavor = "multi_thread")]
async fn unregistered_group_reports_incomplete_and_still_activates_by_name() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let scope = by_id("m2-unregistered-panel");

    let list = c.json("list_actions", json!({"scope": scope})).await;
    assert_eq!(list["complete"], false);
    assert!(!has_action(&list, "panel.ping"), "unregistered groups cannot be enumerated");
    assert!(has_action(&list, "win.save"));

    let r = c.json("activate_action", json!({"scope": scope, "name": "panel.ping"})).await;
    assert_eq!(r["activated"], true);
    assert_eq!(r["observed"]["state_observable"], false);
    assert_eq!(status(&c).await, "m2: pings 1");
    // parameter type unknown: GVariant text
    c.json("activate_action", json!({"scope": scope, "name": "panel.echo", "parameter": "'hi'"})).await;
    assert_eq!(status(&c).await, "m2: echo hi");
    let e = c.err("activate_action", json!({"scope": scope, "name": "panel.echo", "parameter": 5})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    // not reachable from a sibling widget, nor does it exist at all
    let e = c.err("activate_action", json!({"scope": by_id("m2-status"), "name": "panel.ping"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("activate_action", json!({"scope": scope, "name": "panel.missing"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    // state of unregistered groups is not reachable
    let e = c.err("inspect_action", json!({"scope": scope, "name": "panel.ping"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("register_action_group"));
    let e = c.err("change_action_state", json!({"scope": scope, "name": "panel.ping", "value": true})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    assert_eq!(status(&c).await, "m2: echo hi");
}

#[tokio::test(flavor = "multi_thread")]
async fn widget_class_actions_are_listed_with_unknown_enabled() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    // GtkEntry installs misc.menu_entry_icon_* class actions; GtkWindow default.activate / window.close.
    let list = c.json("list_actions", json!({"scope": by_id("m2-shortcut-entry")})).await;
    let class: Vec<&Value> = list["actions"].as_array().unwrap().iter().filter(|a| a["source"]["kind"] == "widget_class").collect();
    assert!(class.iter().all(|a| a["enabled"].is_null()));
    let icon = class.iter().find(|a| a["name"] == "misc.menu_entry_icon_primary").unwrap_or_else(|| panic!("{class:?}"));
    assert_eq!(icon["source"]["owner"], "GtkEntry");
    assert_eq!(icon["parameter_type"], Value::Null);
    assert!(class.iter().any(|a| a["source"]["owner"] == "GtkWindow"), "ancestors' class actions are reachable too");
    let one = c.json("inspect_action", json!({"scope": by_id("m2-shortcut-entry"), "name": "misc.menu_entry_icon_primary"})).await;
    assert_eq!(one["enabled"], Value::Null);
    assert_eq!(one["source"]["kind"], "widget_class");
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_scope_states_are_redacted() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let scope = by_id("m2-secret-entry");
    let mut outputs = Vec::new();
    outputs.push(c.json("list_actions", json!({"scope": scope, "prefix": "vault"})).await);
    outputs.push(c.json("inspect_action", json!({"scope": scope, "name": "vault.token"})).await);
    outputs.push(c.json("change_action_state", json!({"scope": scope, "name": "vault.token", "value": "new-SECRET-2"})).await);
    outputs.push(c.json("activate_action", json!({"scope": scope, "name": "vault.token", "parameter": "new-SECRET-3"})).await);
    outputs.push(c.json("inspect_action_binding", json!({"target": scope})).await);
    assert_eq!(outputs[1]["state"], json!({"redacted": true}));
    assert_eq!(outputs[2]["observed"]["state"], json!({"redacted": true}));
    assert_eq!(outputs[2]["requested"], json!({"redacted": true}));
    let all = serde_json::to_string(&outputs).unwrap();
    assert!(!all.contains("SECRET") && !all.contains(SECRET), "secret leaked: {all}");
}

// ---- menus and bindings ----------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn inspect_menu_walks_sections_submenus_targets_and_attributes() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c.json("inspect_menu", json!({"target": by_id("m2-menu-button")})).await;
    assert_eq!(r["truncated"], false);
    assert!(r["model"]["ref"].as_str().unwrap().starts_with('o'));
    let items = r["menu"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    // section "Edit"
    assert_eq!(items[0]["kind"], "section");
    assert_eq!(items[0]["label"], "Edit");
    let edit = items[0]["section"]["items"].as_array().unwrap();
    assert_eq!(edit[0]["label"], "Save");
    assert_eq!(edit[0]["action"], "win.save");
    assert_eq!(edit[0]["detailed_action"], "win.save");
    assert_eq!(edit[0]["resolved"], json!({"found": true, "enabled": true}));
    assert_eq!(edit[1]["label"], "Limit 5");
    assert_eq!(edit[1]["action"], "win.limit");
    assert_eq!(edit[1]["target"], 5);
    assert_eq!(edit[1]["target_type"], "i");
    assert_eq!(edit[1]["detailed_action"], "win.limit(5)");
    assert_eq!(edit[1]["attributes"]["icon"], "document-save-symbolic");
    // submenu "More" > submenu "Theme" > typed string targets
    assert_eq!(items[1]["kind"], "submenu");
    let more = items[1]["submenu"]["items"].as_array().unwrap();
    assert_eq!(more[0]["kind"], "submenu");
    let theme = more[0]["submenu"]["items"].as_array().unwrap();
    assert_eq!(theme[0]["label"], "Dark");
    assert_eq!(theme[0]["action"], "win.theme");
    assert_eq!(theme[0]["target"], "dark");
    assert_eq!(theme[0]["target_type"], "s");
    assert_eq!(theme[1]["target"], "light");
    assert_eq!(theme[0]["accels"], json!(["<Control><Alt>d"]));
    // a widget-scoped action resolves from the menu's widget
    assert_eq!(more[1]["action"], "reg.toggle");
    // actions that do not exist are reported, not hidden
    assert_eq!(more[2]["resolved"]["found"], false);

    // bounded depth and item count
    let r = c.json("inspect_menu", json!({"target": by_id("m2-menu-button"), "max_depth": 0})).await;
    assert_eq!(r["truncated"], true);
    assert_eq!(r["menu"]["items"][0]["section"]["truncated"], true);
    assert_eq!(r["menu"]["items"][1]["submenu"]["n_items"], 3);
    let r = c.json("inspect_menu", json!({"target": by_id("m2-menu-button"), "max_depth": 1})).await;
    assert_eq!(r["truncated"], true);
    assert_eq!(r["menu"]["items"][0]["section"]["items"][0]["label"], "Save");
    assert_eq!(r["menu"]["items"][1]["submenu"]["items"][0]["submenu"]["truncated"], true);
    let r = c.json("inspect_menu", json!({"target": by_id("m2-menu-button"), "max_items": 2})).await;
    assert_eq!(r["truncated"], true);

    // the model is addressable by ref, and the older fixture menu button works too
    let again = c.json("inspect_menu", json!({"target": r["model"]["ref"]})).await;
    assert_eq!(again["menu"]["items"][0]["label"], "Edit");
    let old = c.json("inspect_menu", json!({"target": by_id("menu-button")})).await;
    let first = &old["menu"]["items"][0]["section"]["items"];
    assert_eq!(first[1]["target"], "dark");
    assert_eq!(first[2]["action"], "app.quit");

    let e = c.err("inspect_menu", json!({"target": by_id("click-counter")})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let e = c.err("inspect_menu", json!({"target": "app"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_action_binding_reports_name_target_and_resolution() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c.json("inspect_action_binding", json!({"target": by_id("m2-theme-button")})).await;
    assert_eq!(r["actionable"], true);
    assert_eq!(r["via"], "GtkActionable");
    assert_eq!(r["action_name"], "win.theme");
    assert_eq!(r["action_target"], "dark");
    assert_eq!(r["detailed_name"], "win.theme::dark");
    assert_eq!(r["resolved"]["found"], true);
    assert_eq!(r["resolved"]["state"], "light");
    assert_eq!(r["resolved"]["target_selected"], false);
    c.json("change_action_state", json!({"name": "win.theme", "value": "dark"})).await;
    let r = c.json("inspect_action_binding", json!({"target": by_id("m2-theme-button")})).await;
    assert_eq!(r["resolved"]["target_selected"], true);

    let m = c.json("inspect_action_binding", json!({"target": by_id("m2-menu-button")})).await;
    assert_eq!(m["menu_model"]["type"], "GMenu");
    assert_eq!(m["menu_model"]["n_items"], 2);
    assert!(m["popover"].is_object());

    let plain = c.json("inspect_action_binding", json!({"target": by_id("click-counter")})).await;
    assert_eq!(plain["actionable"], false);
    assert_eq!(plain["action_name"], Value::Null);
}

// ---- shortcuts -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn inspect_shortcuts_lists_controllers_triggers_actions_and_app_accels() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c.json("inspect_shortcuts", json!({})).await;
    let ctrls = r["controllers"].as_array().unwrap();
    let managed = ctrls.iter().find(|x| x["name"] == "m2-window-shortcuts").unwrap_or_else(|| panic!("{r}"));
    assert_eq!(managed["scope"], "managed");
    assert_eq!(managed["widget"]["type"], "AdwApplicationWindow");
    let sc = managed["shortcuts"].as_array().unwrap();
    assert_eq!(sc[0]["trigger"]["kind"], "keyval");
    assert_eq!(sc[0]["trigger"]["text"], "<Control>s");
    assert_eq!(sc[0]["trigger"]["key_name"], "s");
    assert_eq!(sc[0]["trigger"]["modifiers"], json!(["control-mask"]));
    assert_eq!(sc[0]["action"]["kind"], "named");
    assert_eq!(sc[0]["action"]["action_name"], "win.save");
    assert_eq!(sc[1]["trigger"]["text"], "<Shift><Control>k");
    assert_eq!(sc[1]["action"]["kind"], "callback");
    assert_eq!(sc[2]["trigger"]["kind"], "alternative");
    let local = ctrls.iter().find(|x| x["name"] == "m2-entry-shortcuts").unwrap();
    assert_eq!(local["scope"], "local");
    assert_eq!(local["widget"]["automation_id"], "m2-shortcut-entry");
    assert_eq!(r["total"].as_u64().unwrap() as usize, ctrls.len());
    let accels = r["app_accels"].as_array().unwrap();
    assert!(accels.iter().any(|a| a["action"] == "win.theme::dark" && a["accels"] == json!(["<Control><Alt>d"])), "{accels:?}");

    // scoped to a widget subtree
    let r = c.json("inspect_shortcuts", json!({"target": by_id("m2-shortcut-entry")})).await;
    assert!(r["controllers"].as_array().unwrap().iter().any(|x| x["name"] == "m2-entry-shortcuts"));
    assert!(!r["controllers"].as_array().unwrap().iter().any(|x| x["name"] == "m2-window-shortcuts"));
    assert!(r.get("app_accels").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn activate_shortcut_matches_runs_actions_and_reports_not_found() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    // <Control>s -> win.save (managed controller on the window)
    let r = c.json("activate_shortcut", json!({"trigger": "<Control>s"})).await;
    assert_eq!(r["activated"], true);
    assert_eq!(r["matched"]["source"], "shortcut_controller");
    assert_eq!(r["matched"]["action"]["action_name"], "win.save");
    assert_eq!(r["matched"]["scope"], "managed");
    assert_eq!(r["trigger"], "<Control>s");
    assert_eq!(counter(&c).await, "clicks: 1");

    // callback shortcut
    let r = c.json("activate_shortcut", json!({"trigger": "<Shift><Control>k"})).await;
    assert_eq!(r["matched"]["action"]["kind"], "callback");
    assert_eq!(r["trigger"], "<Shift><Control>k");
    let cb = c.json("inspect", json!({"target": by_id("m2-callback-count")})).await;
    assert_eq!(cb["text"], "m2 callbacks: 1");

    // a local controller matches only from its own widget upward
    let e = c.err("activate_shortcut", json!({"trigger": "<Alt>e"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let r = c.json("activate_shortcut", json!({"trigger": "<Alt>e", "target": by_id("m2-shortcut-entry")})).await;
    assert_eq!(r["matched"]["controller"]["type"], "GtkShortcutController");
    assert_eq!(r["matched"]["scope"], "local");
    let cb = c.json("inspect", json!({"target": by_id("m2-callback-count")})).await;
    assert_eq!(cb["text"], "m2 callbacks: 11");

    // alternative triggers match each alternative; a matched shortcut whose action is not handled is reported
    let r = c.json("activate_shortcut", json!({"trigger": "F6"})).await;
    assert_eq!(r["activated"], false);
    assert_eq!(r["matched"]["handled"], false);

    // application accelerators
    let r = c.json("activate_shortcut", json!({"trigger": "<Control><Alt>d"})).await;
    assert_eq!(r["activated"], true);
    // GTK 4.24 installs the application accelerators as a shortcut controller itself;
    // the app_accel fallback only fires when no controller matched.
    assert!(matches!(r["matched"]["source"].as_str(), Some("shortcut_controller" | "app_accel")), "{r}");
    assert_eq!(c.json("inspect_action", json!({"name": "win.theme"})).await["state"], "dark");

    // nothing matches / not an accelerator
    let e = c.err("activate_shortcut", json!({"trigger": "<Control>x"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = c.err("activate_shortcut", json!({"trigger": "not a trigger"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    assert_eq!(counter(&c).await, "clicks: 1");
}

// ---- controllers -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn inspect_controllers_lists_gestures_with_phase_and_configuration() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    let r = c.json("inspect_controllers", json!({"target": by_id("m2-clickable")})).await;
    assert_eq!(r["total"].as_u64().unwrap() as usize, r["controllers"].as_array().unwrap().len());
    let click = r["controllers"].as_array().unwrap().iter().find(|x| x["name"] == "m2-click").unwrap_or_else(|| panic!("{r}"));
    assert_eq!(click["type"], "GtkGestureClick");
    assert_eq!(click["propagation_phase"], "capture");
    assert_eq!(click["propagation_limit"], "same-native");
    assert_eq!(click["gesture"]["button"], 3);
    assert_eq!(click["gesture"]["n_points"], 1);
    assert_eq!(click["gesture"]["exclusive"], true);
    assert_eq!(click["gesture"]["touch_only"], false);
    assert!(click["ref"].as_str().unwrap().starts_with('o'));

    // a button owns its click gesture
    let r = c.json("inspect_controllers", json!({"target": by_id("save-primary")})).await;
    let g = r["controllers"].as_array().unwrap().iter().find(|x| x["type"] == "GtkGestureClick").unwrap_or_else(|| panic!("{r}"));
    assert!(g["propagation_phase"].is_string());
    assert_eq!(g["gesture"]["n_points"], 1);

    // shortcut controllers carry their scope
    let r = c.json("inspect_controllers", json!({"target": {"type_name": "GtkWindow"}})).await;
    let s = r["controllers"].as_array().unwrap().iter().find(|x| x["name"] == "m2-window-shortcuts").unwrap();
    assert_eq!(s["scope"], "managed");
    assert_eq!(s["n_shortcuts"], 3);
}

// ---- gates -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn mutating_action_tools_are_gated_in_broker_and_agent() {
    let display = require_display!();
    let calls: Vec<(&str, Value)> = vec![
        ("activate_action", json!({"name": "win.save"})),
        ("change_action_state", json!({"name": "win.limit", "value": 3})),
        ("activate_shortcut", json!({"trigger": "<Control>s"})),
    ];
    // broker refuses
    let fx = Fixture::start(&display, true, &[]);
    let ro = Client { broker: broker(&fx, false) };
    for (t, a) in &calls {
        let e = ro.err(t, a.clone()).await;
        assert_eq!(e.code, ErrorCode::MutationsDisabled, "{t} (broker)");
    }
    // agent refuses (GTK_MCP_MUTATIONS unset in the app)
    let fx2 = Fixture::start(&display, false, &[]);
    let c2 = Client { broker: broker(&fx2, true) };
    for (t, a) in &calls {
        let e = c2.err(t, a.clone()).await;
        assert_eq!(e.code, ErrorCode::MutationsDisabled, "{t} (agent)");
    }
    // read-only tools keep working without mutations, and nothing ran
    let list = c2.json("list_actions", json!({"scope": "window"})).await;
    assert!(has_action(&list, "win.save"));
    assert_eq!(c2.json("inspect_action", json!({"name": "win.limit"})).await["state"], 0);
    assert_eq!(counter(&c2).await, "clicks: 0");
    c2.json("inspect_shortcuts", json!({})).await;
    c2.json("inspect_controllers", json!({"target": by_id("m2-clickable")})).await;
    c2.json("inspect_menu", json!({"target": by_id("m2-menu-button")})).await;
    // a batch is mutating exactly when a step is
    let r = c2.json("batch", json!({"steps": [{"tool": "list_actions", "arguments": {"scope": "window"}}]})).await;
    assert_eq!(r["steps"][0]["ok"], true, "{r}");
    let e = c2.err("batch", json!({"steps": [{"tool": "activate_action", "arguments": {"name": "win.save"}}]})).await;
    assert_eq!(e.code, ErrorCode::MutationsDisabled);
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_run_inside_batch() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c
        .json(
            "batch",
            json!({"steps": [
                {"tool": "activate_action", "arguments": {"name": "win.save"}},
                {"tool": "change_action_state", "arguments": {"name": "win.limit", "value": 4}},
                {"tool": "inspect_action", "arguments": {"name": "win.limit"}}
            ]}),
        )
        .await;
    assert!(r["steps"].as_array().unwrap().iter().all(|s| s["ok"] == true), "{r}");
    assert_eq!(counter(&c).await, "clicks: 1");
    assert_eq!(c.json("inspect_action", json!({"name": "win.limit"})).await["state"], 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_toolset_is_absent_under_the_default_core_toolset() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let names = ["list_actions", "inspect_action", "activate_action", "change_action_state", "inspect_action_binding", "inspect_menu", "inspect_shortcuts", "activate_shortcut", "inspect_controllers"];
    let core = Broker::new(Policy { allow_mutations: true, ..Policy::default() }, Config::with_socket_dir(fx.dir.path())).unwrap();
    for n in names {
        assert!(!core.is_enabled(n), "{n} must not be exposed by --toolsets core");
    }
    let c = Client { broker: core };
    let e = c.err("list_actions", json!({"scope": "window"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert!(e.message.contains("unknown tool"), "{}", e.message);

    let both = broker(&fx, true);
    for n in names {
        assert!(both.is_enabled(n), "{n}");
    }
    // --tools narrows within a toolset
    let only = Broker::new(
        Policy { toolsets: vec!["actions".into()], tools: Some(vec!["list_actions".into()]), ..Policy::default() },
        Config::with_socket_dir(fx.dir.path()),
    )
    .unwrap();
    assert_eq!(only.enabled_tools().iter().map(|t| t.name).collect::<Vec<_>>(), vec!["list_actions"]);
    // mutating flags
    let mutating: Vec<&str> = both.enabled_tools().iter().filter(|t| t.toolset == "actions" && t.mutating).map(|t| t.name).collect();
    assert_eq!(mutating, vec!["activate_action", "change_action_state", "activate_shortcut"]);
}
