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
    /// types, the bar and the cache in memory its patterns search over, a
    /// path as a value, and the thread pool.
    const LINT: AllowList = AllowList {
        modules: &["lint", "code"],
        store_items: &[
            "Anchor",
            "Bar",
            "Decision",
            "PatternCache",
            "ProjectState",
            "QualityFault",
            "Shortfall",
            "Status",
        ],
        std_items: &[("path", "PathBuf")],
        crates: &["rayon"],
    };

    #[test]
    fn the_lint_module_names_nothing_but_what_its_allow_list_holds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        // Without `rayon`, the check faults the passes' thread pool: it
        // reads lint's files.
        let without = AllowList {
            crates: &[],
            ..LINT
        };
        assert_ne!(faults(root, "src/lint", &without), Vec::<String>::new());

        assert_eq!(faults(root, "src/lint", &LINT), Vec::<String>::new());
    }
}
