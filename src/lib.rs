pub(crate) mod budget;
pub mod cli;

pub(crate) mod code;
pub(crate) mod commands;
pub(crate) mod console;
pub(crate) mod discovery;
pub(crate) mod error;
pub(crate) mod git;
pub(crate) mod mcp;
pub(crate) mod sources;
pub mod store;

pub use error::{Error, Result};

/// What `benches/` drives: the MCP server in process, over a store written
/// by the seeded [`Corpus`](bench::Corpus), and the code index. Built only
/// with the `bench` feature.
#[cfg(feature = "bench")]
pub mod bench {
    pub use crate::mcp::Server;
    pub use crate::store::corpus::Corpus;

    use crate::store::{ProjectState, Store};

    /// Resolve every anchor of `state` against `store`'s code root, as the
    /// anchor findings do; the number of anchors resolved.
    pub fn resolve_anchors(store: &Store, state: &ProjectState) -> usize {
        crate::sources::resolve_anchors(store, state).len()
    }
}
