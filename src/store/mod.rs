//! The decision graph on disk: `.trurlic/` read into a [`ProjectState`],
//! validated, and written through a journaled commit, a [`Writer`]'s for
//! every write but `init` and `migrate`, which under `shared` placement
//! ends with a commit to the graph's own git repository.
//!
//! Its parts depend one way, each naming only those before it: `model`
//! holds the values; `graph` judges a loaded state; `format` reads and
//! writes the files and holds the steps from older formats; `write` opens
//! the graph under its lock and commits to it; `quality`, `apply`, `cache`
//! and `corpus` build on those. Code outside the store names its items
//! through the re-exports here.

mod apply;
mod cache;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod corpus;
mod format;
mod graph;
mod model;
mod quality;
#[cfg(test)]
pub(crate) mod testing;
mod write;

pub(crate) use apply::{
    AddComponent, AnchorInput, Applied, Batch, Done, Field, LeftRepair, ListEdit, Merge, Op,
    OpKind, PatternNote, Record, Refusal, Remove, RemoveComponent, Rename, Repair, RepairAnchors,
    Repaired, Revise, ReviseComponent, Warning, WarningKind,
};
pub(crate) use cache::SymbolsKey;
#[cfg(test)]
pub(crate) use corpus::seeded;
pub use format::Store;
pub(crate) use format::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
pub(crate) use format::{PlacementDoubt, StorePathFault};
pub(crate) use graph::{
    Issue, IssueKind, LinkedGraph, MainWorktree, Named, PlacementFacts, WorkTreeFacts, anchors_in,
    normalize_path,
};
pub(crate) use model::limits;
pub(crate) use model::{
    Anchor, AnchorFault, Component, Decision, FormatVersion, Level, PROJECT_COMPONENT,
    PROJECT_FILE, Quality, STORE_DIR, Status, StatusName, TextFault, TextField, Timestamp,
    TimestampError,
};
pub use model::{FORMAT_VERSION, Placement, ProjectState};
pub(crate) use quality::Bar;
pub(crate) use quality::{PreparedBar, QualityFault, Shortfall};
pub(crate) use write::{HistoryFinding, Writer};
pub(crate) use write::{failpoint, sync_dir};

#[cfg(test)]
pub(crate) use apply::{NodeKind, OpAt, Rule};
#[cfg(test)]
pub(crate) use model::{
    AcceptedText, DecisionField, NameFault, NodeFile, Pattern, STATE_DIR, WordRange,
};
#[cfg(test)]
pub(crate) use quality::PatternMatch;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// The parts of the store, each after every part its code may name.
    const PARTS: [&str; 8] = [
        "model", "graph", "format", "write", "quality", "apply", "cache", "corpus",
    ];

    /// Append the non-test code of every Rust file at `path` to `code`: the
    /// text before a file's `tests` or `testing` module, nothing of a
    /// `tests.rs`, and no comment line.
    fn code_under(path: &Path, code: &mut String) {
        if path.ends_with("tests.rs") {
            return;
        }
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                code_under(&entry.unwrap().path(), code);
            }
            return;
        }
        let text = fs::read_to_string(path).unwrap();
        let end = [
            "#[cfg(test)]\nmod tests",
            "#[cfg(test)]\npub(in crate::store) mod testing",
        ]
        .iter()
        .filter_map(|module| text.find(module))
        .min()
        .unwrap_or(text.len());
        let lines = text[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"));
        code.extend(lines.flat_map(|line| [line, "\n"]));
    }

    /// A part names the store's items through the parts it may name, never
    /// through the re-exports of the store's root, which hide the part an
    /// item belongs to. A call of a `Store` method another part defines
    /// names no path, so this test does not see it.
    #[test]
    fn each_part_of_the_store_names_only_the_parts_before_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
        for (index, part) in PARTS.iter().enumerate() {
            let dir = root.join(part);
            let file = root.join(format!("{part}.rs"));
            let mut code = String::new();
            code_under(if dir.is_dir() { &dir } else { &file }, &mut code);
            for (at, _) in code.match_indices("crate::store::") {
                let named: String = code[at + "crate::store::".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    PARTS[..=index].contains(&named.as_str()),
                    "{part} names `crate::store::{named}`"
                );
            }
        }
    }
}
