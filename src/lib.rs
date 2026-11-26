//! Trurlic as a library: the binary's entry [`main`], the graph format
//! the binary writes ([`FORMAT_VERSION`], which integration tests name)
//! and, with the `bench` feature, the module `bench`. No module is public,
//! so the compiler reports what nothing in the crate uses.

pub(crate) mod budget;
pub(crate) mod cli;
pub(crate) mod code;
pub(crate) mod commands;
pub(crate) mod console;
pub(crate) mod discovery;
pub(crate) mod error;
pub(crate) mod failpoint;
pub(crate) mod git;
pub(crate) mod lint;
pub(crate) mod mcp;
#[cfg(test)]
mod purity;
pub(crate) mod sources;
pub(crate) mod store;
pub(crate) mod tools;

pub use cli::main;
pub(crate) use error::{Error, Result};
pub use store::FORMAT_VERSION;

/// What `benches/` drives: the MCP server in process, over a store written
/// by the seeded [`Corpus`](bench::Corpus), the code index and the likely
/// duplicates pass, and the store types its setup names. Built only with
/// the `bench` feature.
#[cfg(feature = "bench")]
pub mod bench {
    pub use crate::mcp::Server;
    pub use crate::store::corpus::Corpus;
    pub use crate::store::{Placement, ProjectState, Store};

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
        Similarity::of(&state.decisions)
            .pairs(Among::Every, usize::MAX, usize::MAX)
            .total
    }
}
