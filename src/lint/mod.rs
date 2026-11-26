//! Findings over the graph that refuse nothing: what a reader should look
//! at. Pure: each part reads the graph's decisions and the facts handed to
//! it, and touches no file and no clock. It is handed no
//! [`ProjectState`](crate::store::ProjectState), whose code root is a path
//! whose methods read the file system.

pub(crate) mod anchors;
pub(crate) mod duplicates;
pub(crate) mod quality;

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::Path;

    use crate::purity::{AllowList, Crate};

    /// What `lint` may name: the code index's facts, the graph's value
    /// types, the bar, and the thread pool's
    /// parallel iterators, whose schedule no result depends on (the passes'
    /// tests at one thread and eight hold it). None of them holds a path.
    const LINT: AllowList = AllowList {
        modules: &["lint", "code"],
        store_items: &["Anchor", "Bar", "Decision", "Shortfall", "Status"],
        std_items: &[],
        crates: &[],
        crate_items: &[("rayon", "prelude")],
    };

    /// `crate::purity` states what the check cannot see.
    #[test]
    pub(crate) fn the_lint_module_names_nothing_but_what_its_allow_list_holds() {
        let source = Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        // Without the parallel iterators, the check faults the passes'
        // thread pool: it reads lint's files.
        let without = AllowList {
            crate_items: &[],
            ..LINT
        };
        assert_ne!(source.faults("lint", &without), Vec::<String>::new());

        assert_eq!(source.faults("lint", &LINT), Vec::<String>::new());
    }

    /// A read of the file system through the code root the loaded graph
    /// holds names the graph's type, which the list refuses.
    #[test]
    fn lint_reaches_no_path_the_graph_holds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("Cargo.toml", "[package]\n");
        write("src/lib.rs", "mod lint;\n");
        write(
            "src/lint/mod.rs",
            "use crate::store::ProjectState;\n\
             pub(crate) fn listed(state: &ProjectState) -> usize {\n    \
             state.code_root.read_dir().map_or(0, Iterator::count)\n}\n",
        );

        let found = Crate::read(root).faults("lint", &LINT);

        assert_eq!(
            found,
            ["src/lint/mod.rs:1: names `crate::store::ProjectState`"]
        );
    }
}
