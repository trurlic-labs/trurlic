//! The way from an older format to the current one, as data: the registry
//! of steps and the tree of tables they rewrite. Carrying a migration out
//! on a store is `store::write::migrate`'s.
//!
//! Every graph file is read as a TOML table into a [`Tree`](tree::Tree)
//! (`tree.rs`). The chain of steps of the registry (`steps.rs`) from the
//! store's version rewrites the tree one format at a time, each recording
//! what it drops, a [`Loss`] carrying the dropped value's text, and what
//! else the owner must be able to read after the fact, a [`Fact`]. The
//! tree is then read as the current format, which refuses any key it has
//! no place for, so a step must map or drop every value it meets.

mod format_0_5;
mod placement;
mod report;
mod steps;
#[cfg(test)]
pub(in crate::store) mod testing;
mod tree;

pub(crate) use placement::PlacementDoubt;
pub(crate) use placement::PlacementSource;
pub(crate) use report::{Fact, Loss, MigrationReport, PatternRecord, PlacementBasis};
pub(in crate::store) use steps::{STEPS, Step, chain, names, walk};
pub(in crate::store) use tree::{File, PATTERNS_DIR, version_key};

#[cfg(test)]
pub(in crate::store) use report::{RefText, StepReport};
#[cfg(test)]
pub(in crate::store) use tree::{GRAPH_FILE, Tree};
