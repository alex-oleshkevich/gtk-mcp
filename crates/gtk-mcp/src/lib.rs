//! gtk-mcp broker: MCP server that talks to agents embedded in GTK4 apps
//! (`docs/plan.md`). The library holds everything except stdio plumbing so
//! tests can drive [`Broker::call_tool`] directly.

pub mod a11y;
pub mod broker;
pub mod diff;
pub mod launch;
pub mod lifecycle;
pub mod policy;
pub mod ring;
pub mod server;
pub mod session;

pub use a11y::atspi::BusConfig;
pub use broker::{Broker, Content, MAX_BATCH_STEPS, ToolOutcome};
pub use lifecycle::StateResource;
pub use launch::{ExitInfo, Launch, LaunchSpec};
pub use policy::{Config, Policy};
pub use ring::{Entry, Read};
pub use session::{Progress, ProgressSink, Session};
