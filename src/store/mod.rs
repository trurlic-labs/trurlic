//! The decision graph on disk: `.trurlic/` read into a [`ProjectState`],
//! validated, and written through a journaled commit, a [`Writer`]'s for
//! every write but `init` and `migrate`, which under `shared` placement
//! ends with a commit to the graph's own git repository.
//!
//! Its parts depend one way, each naming only those before it: `model`
//! holds the files' values; `graph` the loaded state and its judgements;
//! `format` reads and writes the files and holds the steps from older
//! formats; `quality` compiles the bar a write holds a decision to;
//! `write` opens the graph under its lock and commits to it; `apply`,
//! `cache` and `corpus` build on those. Code outside the store names its
//! items through the re-exports here.

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
pub use graph::ProjectState;
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
pub use model::{FORMAT_VERSION, Placement};
pub(crate) use quality::Bar;
pub(crate) use quality::{PreparedBar, QualityFault, Shortfall};
pub(crate) use write::sync_dir;
pub(crate) use write::{HistoryFinding, Writer};

#[cfg(test)]
pub(crate) use apply::{NodeKind, OpAt, Rule};
#[cfg(test)]
pub(crate) use graph::NodeFile;
#[cfg(test)]
pub(crate) use model::{AcceptedText, DecisionField, NameFault, Pattern, STATE_DIR, WordRange};
#[cfg(test)]
pub(crate) use quality::PatternMatch;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::purity::places_beyond;

    /// The parts of the store, each after every part its code may name.
    const PARTS: [&str; 8] = [
        "model", "graph", "format", "quality", "write", "apply", "cache", "corpus",
    ];

    /// What every part may name outside the store: the error, the console,
    /// git and the failpoints.
    const OUTSIDE: [&str; 6] = ["Error", "Result", "error", "console", "git", "failpoint"];

    /// A part names the store's items through the parts it may name, never
    /// through the re-exports of the store's root, which hide the part an
    /// item belongs to, and names nothing outside the store but
    /// [`OUTSIDE`]. A method called on a value names no path, so a call of
    /// a method of `Store` that a later part defines is not seen.
    #[test]
    fn each_part_of_the_store_names_only_the_parts_before_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (index, part) in PARTS.iter().enumerate() {
            let before: Vec<String> = PARTS[..=index]
                .iter()
                .map(|part| format!("store::{part}"))
                .collect();
            let within: Vec<&str> = before.iter().map(String::as_str).chain(OUTSIDE).collect();

            let found = places_beyond(root, &format!("store::{part}"), &within);

            assert_eq!(found, Vec::<String>::new(), "{part}");
        }
    }
}
