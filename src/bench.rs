//! What `benches/` drives: the MCP server in process, over a store written
//! by the seeded [`Corpus`], the anchor findings of `status` and of
//! `check`, the likely duplicates pass, and the store types its setup
//! names. Built only with the `bench` feature.

use crate::lint::anchors::{Checked, Kind};

pub use crate::mcp::Server;
pub use crate::store::corpus::Corpus;
pub use crate::store::{Placement, ProjectState, Store};

/// What the repository's git made of the anchors `check` finds missing
/// in the calling tree.
#[derive(Debug, Default)]
pub struct GitVerdicts {
    /// Stale anchors that git's history repairs.
    pub repaired: usize,
    /// Anchors that another worktree holds.
    pub pending: usize,
}

/// The anchor findings of every decision of `state` in `store`'s code
/// root, as `status` gathers them, running no git; how many there are.
#[must_use]
pub fn tree_findings(store: &Store, state: &ProjectState) -> usize {
    crate::sources::tree_findings(store, &state.decisions).len()
}

/// The anchor findings of every decision of `state` in `store`'s code
/// root, as `check` gathers them with the repository's git; what that
/// git made of the missing anchors.
#[must_use]
pub fn checked_findings(store: &Store, state: &ProjectState) -> GitVerdicts {
    let mut verdicts = GitVerdicts::default();
    for finding in crate::sources::checked_findings(store, &state.decisions).findings {
        match finding.kind {
            Kind::Missing(_, Checked::Stale { repair: Some(_) }) => verdicts.repaired += 1,
            Kind::Missing(_, Checked::Pending(_)) => verdicts.pending += 1,
            Kind::Missing(_, Checked::Stale { repair: None } | Checked::Planned)
            | Kind::Malformed(_)
            | Kind::Unverified(_)
            | Kind::Ambiguous(_)
            | Kind::Weak(_) => {}
        }
    }
    verdicts
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
