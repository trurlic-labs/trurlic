//! The decision graph on disk: `.trurlic/` read into a [`ProjectState`],
//! validated, and written through a journaled commit, a [`Writer`]'s for
//! every write but `init` and `migrate`, which under `shared` placement ends with a commit to the graph's own
//! git repository (`history.rs`).

mod access;
mod apply;
mod commit;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod corpus;
mod create;
mod durable;
pub(crate) mod failpoint;
mod format;
mod graph;
mod history;
mod journal;
mod lock;
mod migrate;
mod model;
mod quality;
mod recovery;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod seeded;
mod symbols;

pub use access::Writer;
pub use apply::{
    AddComponent, AnchorInput, Applied, Batch, Done, Field, LeftRepair, ListEdit, Merge, Op,
    OpKind, PatternNote, Record, Refusal, Remove, RemoveComponent, Rename, Repair, RepairAnchors,
    Repaired, Revise, ReviseComponent, Warning, WarningKind,
};
pub(crate) use durable::sync_dir;
pub(crate) use format::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
pub use format::{PlacementDoubt, Store, StorePathFault};
pub use graph::{
    Issue, IssueKind, LinkedGraph, MainWorktree, Named, PlacementFacts, WorkTreeFacts, anchors_in,
    normalize_path,
};
pub use history::HistoryFinding;
pub use lock::StoreLock;
pub(crate) use model::limits;
pub use model::{
    Anchor, AnchorFault, COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, FORMAT_VERSION,
    FormatVersion, Level, PROJECT_COMPONENT, PROJECT_FILE, Placement, ProjectState, Quality,
    STORE_DIR, Status, StatusName, TextFault, TextField, Timestamp, TimestampError,
};
pub(crate) use quality::Bar;
pub use quality::{PreparedBar, QualityFault, Shortfall};
pub use symbols::SymbolsKey;

#[cfg(test)]
pub(crate) use apply::{NodeKind, OpAt, Rule};
#[cfg(test)]
pub(crate) use model::{
    AcceptedText, DecisionField, NameFault, NodeFile, Pattern, STATE_DIR, WordRange,
};
#[cfg(test)]
pub(crate) use quality::PatternMatch;

#[cfg(test)]
pub(crate) mod testing;
