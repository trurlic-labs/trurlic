pub(crate) mod budget;
pub mod cli;

pub(crate) mod code;
pub(crate) mod commands;
pub(crate) mod console;
pub(crate) mod discovery;
pub(crate) mod error;
pub(crate) mod git;
pub(crate) mod lint;
pub(crate) mod mcp;
pub(crate) mod sources;
pub mod store;

pub use error::{Error, Result};

/// What `benches/` drives: the MCP server in process, over a store written
/// by the seeded [`Corpus`](bench::Corpus), the code index and the likely
/// duplicates pass. Built only with the `bench` feature.
#[cfg(feature = "bench")]
pub mod bench {
    pub use crate::mcp::Server;
    pub use crate::store::corpus::Corpus;

    use crate::store::{ProjectState, Store};

    /// The anchor findings of every decision of `state` in `store`'s code
    /// root, as `check` gathers them; how many there are.
    #[must_use]
    pub fn anchor_findings(store: &Store, state: &ProjectState) -> usize {
        crate::sources::checked_findings(store, &state.decisions)
            .findings
            .len()
    }

    /// Whether the query of the language named `name` compiles, compiled
    /// afresh, as a process compiles it on its first file of the language.
    #[must_use]
    pub fn query_compiles(name: &str) -> bool {
        crate::code::query_compiles(name)
    }

    /// The likely duplicate pairs of `state`, the pass over every pair
    /// that `status` makes, unbounded: how many there are.
    #[must_use]
    pub fn likely_duplicates(state: &ProjectState) -> usize {
        use crate::lint::duplicates::{Among, Similarity};
        Similarity::of(state, usize::MAX).map_or(0, |similarity| {
            similarity.page(Among::Every, None, usize::MAX).total
        })
    }
}
