//! Wire contract between the gtk-mcp broker and the embedded agent
//! (`docs/plan.md` sections 7 and 8). This crate is the only coupling between
//! the two.

pub mod rpc;
pub mod snapshot;
pub mod tools;
pub mod types;

pub use rpc::*;
pub use tools::*;
pub use types::*;
