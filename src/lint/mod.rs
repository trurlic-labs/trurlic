//! Findings over the graph that refuse nothing: what a reader should look
//! at. Pure: each part reads a [`ProjectState`](crate::store::ProjectState)
//! and the facts handed to it, and touches no file and no clock.

pub(crate) mod anchors;
pub(crate) mod duplicates;
pub(crate) mod quality;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::code::{AllowList, faults};

    /// What `lint` may name: the code index's facts, the graph's value
    /// types, the bar, the store's constant limits, and the thread pool's
    /// parallel iterators,
    /// whose schedule no result depends on (the passes' tests at one thread
    /// and eight hold it).
    const LINT: AllowList = AllowList {
        modules: &["lint", "code"],
        store_items: &[
            "Anchor",
            "Bar",
            "Decision",
            "ProjectState",
            "QualityFault",
            "Shortfall",
            "Status",
            "limits",
        ],
        std_items: &[],
        crates: &[],
        crate_items: &[("rayon", "prelude")],
    };

    #[test]
    fn the_lint_module_names_nothing_but_what_its_allow_list_holds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        // Without the parallel iterators, the check faults the passes'
        // thread pool: it reads lint's files.
        let without = AllowList {
            crate_items: &[],
            ..LINT
        };
        assert_ne!(faults(root, "src/lint", &without), Vec::<String>::new());

        assert_eq!(faults(root, "src/lint", &LINT), Vec::<String>::new());
    }
}
