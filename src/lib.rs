//! Trurlic as a library: the binary's entry [`main`], the graph format
//! the binary writes ([`FORMAT_VERSION`], which integration tests name)
//! and, with the `bench` feature, the module `bench`. No module is public,
//! so the compiler reports what nothing in the crate uses.

#[cfg(feature = "bench")]
pub mod bench;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::code::CodeTree;
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

    /// Each module of the module map in `CLAUDE.md`, and what its non-test
    /// code may name in the crate besides itself: the modules its arrow
    /// reaches, of `mcp` only `run_server` for `commands`, and `Error`,
    /// `Result`, `console` and `failpoint` where the map says it uses them.
    const MAP: [(&str, &[&str]); 15] = [
        ("store", &["git", "Error", "Result", "console", "failpoint"]),
        ("code", &["store"]),
        ("lint", &["store", "code"]),
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

    /// The module of the crate root an item of a row of [`MAP`] is in:
    /// `Error` and `Result` are `error`'s, `run_server` is `mcp`'s.
    fn module_of(item: &str) -> &str {
        match item {
            "Error" | "Result" => "error",
            item => item.split("::").next().unwrap_or(item),
        }
    }

    /// The edges between the modules of `src` that its non-test code's
    /// imports and re-exports make, as the code index resolves them over
    /// this repository's tree, are the arrows of [`MAP`], no more and no
    /// fewer; the root, which re-exports from its modules, is no module of
    /// the map. Where purity holds each path to its row, this holds what
    /// the index resolves, re-exports followed.
    #[test]
    fn the_imports_between_the_modules_of_src_are_the_arrows_of_the_map() {
        let (facts, _) = crate::sources::testing::this_repository();
        let tree = CodeTree::of(facts);

        let (imported, _) = tree.code_imports("src", "");

        let arrows: BTreeSet<(&str, &str)> = MAP
            .iter()
            .flat_map(|(module, items)| items.iter().map(|item| (*module, module_of(item))))
            .filter(|arrow| arrow.0 != arrow.1)
            .collect();
        let unused: Vec<_> = arrows.difference(&imported).collect();
        let unmapped: Vec<_> = imported.difference(&arrows).collect();
        assert!(
            unused.is_empty() && unmapped.is_empty(),
            "arrows no import makes: {unused:?}\nimports no arrow holds: {unmapped:?}"
        );
    }

    /// Every import under `src`, of test code or not, resolves: the one
    /// claim over this repository that the index alone can state, as no
    /// boundary test resolves a path through re-exports.
    #[test]
    fn every_import_under_src_resolves() {
        let (facts, _) = crate::sources::testing::this_repository();
        let tree = CodeTree::of(facts);

        let (_, unresolved) = tree.code_imports("src", "");

        assert_eq!(unresolved, 0);
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
