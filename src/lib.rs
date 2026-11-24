pub(crate) mod budget;
pub mod cli;

pub(crate) mod commands;
pub(crate) mod console;
pub(crate) mod discovery;
pub(crate) mod error;
pub(crate) mod git;
pub(crate) mod mcp;
pub mod store;

pub use error::{Error, Result};

/// What `benches/` drives: the MCP server in process, over a store written
/// by the seeded [`Corpus`](bench::Corpus). Built only with the `bench`
/// feature.
#[cfg(feature = "bench")]
pub mod bench {
    pub use crate::mcp::Server;
    pub use crate::store::corpus::Corpus;
}
