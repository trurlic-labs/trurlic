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
/// by the seeded [`Corpus`](bench::Corpus), the anchor findings of `status`
/// and of `check`, the likely duplicates pass, and the store types its
/// setup names. Built only with the `bench` feature.
#[cfg(feature = "bench")]
pub mod bench {
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::purity::Crate;

    /// Each test in a module's root that holds the module to a stricter
    /// rule of its own, by the module's name: `code` and `lint` to their
    /// allow lists, the parts of `code` and of `store` to one order. A
    /// test is named by its path, so one deleted fails to compile here.
    const HELD: [(&str, fn()); 4] = [
        (
            "code",
            crate::code::tests::the_index_names_nothing_but_what_its_allow_list_holds,
        ),
        (
            "code",
            crate::code::tests::each_sub_module_names_no_module_above_it,
        ),
        (
            "lint",
            crate::lint::tests::the_lint_module_names_nothing_but_what_its_allow_list_holds,
        ),
        (
            "store",
            crate::store::tests::each_part_of_the_store_names_only_the_parts_before_it,
        ),
    ];

    /// Each other module of the module map in `CLAUDE.md`, and what its
    /// non-test code may name in the crate besides itself: the modules its
    /// arrow reaches, of `mcp` only `run_server` for `commands`, and `Error`,
    /// `Result`, `console` and `failpoint` where the map says it uses them.
    const MAP: [(&str, &[&str]); 12] = [
        (
            "tools",
            &[
                "store",
                "lint",
                "sources",
                "budget",
                "discovery",
                "Error",
                "console",
                "failpoint",
            ],
        ),
        (
            "mcp",
            &["tools", "budget", "Result", "console", "failpoint"],
        ),
        (
            "commands",
            &[
                "store",
                "tools",
                "mcp::run_server",
                "sources",
                "lint",
                "git",
                "discovery",
                "budget",
                "Error",
                "Result",
                "console",
            ],
        ),
        (
            "cli",
            &["commands", "store", "tools", "Error", "Result", "console"],
        ),
        ("discovery", &["store", "git", "Error", "Result"]),
        (
            "sources",
            &["code", "lint", "store", "git", "Error", "Result", "console"],
        ),
        ("git", &["Error", "Result"]),
        ("budget", &[]),
        ("error", &["store"]),
        ("console", &["Error", "Result"]),
        ("failpoint", &["console"]),
        ("bench", &["mcp", "store", "sources", "code", "lint"]),
    ];

    /// A module added to the crate, in a file or inline in this one, fails
    /// here until a row of [`HELD`] or of [`MAP`] holds it.
    #[test]
    fn every_module_is_held_by_a_test_of_its_own_or_a_row_of_the_map() {
        let source = Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        let rows = MAP.iter().map(|(module, _)| *module);

        let held: BTreeSet<&str> = HELD.iter().map(|(module, _)| *module).chain(rows).collect();

        assert_eq!(source.root_modules(), held);
    }

    /// Every path the non-test code of a module of [`MAP`] names into the
    /// crate goes into its row, however it is spelled; `crate::purity`
    /// states what the check cannot see.
    #[test]
    fn each_module_of_the_map_names_only_what_its_arrow_reaches() {
        let source = Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        for (module, named) in MAP {
            let within: Vec<&str> = std::iter::once(module)
                .chain(named.iter().copied())
                .collect();

            let found = source.places_beyond(module, &within);

            assert_eq!(found, Vec::<String>::new(), "{module}");
        }
    }

    /// No module re-exports an item of another, so a caller names each
    /// item through its own module, which the caller's row must reach.
    /// `bench` alone may: it is the crate's surface for `benches/`, and
    /// no row reaches it. `crate::purity` states what the check cannot see.
    #[test]
    fn no_module_re_exports_an_item_of_another() {
        let source = Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        for module in source.root_modules() {
            if module == "bench" {
                continue;
            }

            let found = source.re_exports_beyond(module);

            assert_eq!(found, Vec::<String>::new(), "{module}");
        }
    }
}
