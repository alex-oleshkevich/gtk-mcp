//! Embedded GTK agent (`docs/plan.md` 4.2, `docs/embedding.md`).
//!
//! One call, on the main thread after the application object exists:
//! `gtk_mcp_agent::attach(&app)`. The agent is inert unless `GTK_MCP=1`.

pub mod agent;
pub mod config;
pub mod json;
pub mod layer;
pub mod registry;
pub mod secrecy;
pub mod selector;
pub mod socket;
pub mod state;
pub mod tools;
pub mod widgets;

use agent::Agent;
use gtk4 as gtk;
use gtk4::prelude::*;

pub use secrecy::mark_secret;
pub use state::{register_settings, register_state};

/// Attach the agent to `app`. Must be called on the main thread after
/// `gtk::init`. Inert (and idempotent) unless `GTK_MCP=1`; always removes
/// `GTK_MCP*` from the process environment so child processes are not
/// instrumented.
pub fn attach(app: &impl IsA<gtk::Application>) {
    let Some(config) = config::Config::take_from_env() else { return };
    if Agent::current().is_some() {
        return;
    }
    // GTK may legitimately not be initialized yet (plain gtk-rs apps initialize it in
    // `GApplication::startup`); the executor re-checks before every request.
    agent::ensure_gtk_rs_initialized();
    let app: gtk::Application = app.upcast_ref::<gtk::Application>().clone();
    let (tx, rx) = async_channel::unbounded::<socket::Incoming>();
    let agent = Agent::new(app.clone(), config);
    agent.install();

    // Executor: lives on the default main context and runs only while the
    // main loop iterates; earlier requests wait in the channel.
    glib::spawn_future_local(agent::executor(agent.clone(), rx));

    let bind = {
        let agent = agent.clone();
        move || {
            if agent.socket_path().is_some() {
                return;
            }
            match socket::start(&agent.config.dir, tx.clone()) {
                Ok(h) => agent.set_socket(h),
                Err(e) => eprintln!("gtk-mcp: cannot bind agent socket in {}: {e}", agent.config.dir.display()),
            }
        }
    };
    // A remote GApplication instance never emits `startup` and never binds.
    if app.is_registered() {
        if !app.is_remote() {
            bind();
        }
    } else {
        app.connect_startup(move |_| bind());
    }
    let agent_for_shutdown = agent.clone();
    app.connect_shutdown(move |_| agent_for_shutdown.cleanup());
}
