#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub(crate) mod budget;
pub mod cli;

pub(crate) mod commands;
pub(crate) mod error;
pub(crate) mod map;
pub(crate) mod mcp;
pub mod store;
pub(crate) mod workflow;

pub use error::{Error, Result};

/// What `benches/` drives: the MCP server in process, over a store written
/// by [`store::corpus`]. Built only with the `bench` feature.
#[cfg(feature = "bench")]
pub mod bench {
    pub use crate::mcp::Server;
}
