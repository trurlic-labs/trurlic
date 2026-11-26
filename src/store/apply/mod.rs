//! `apply`, the one write of the graph: a batch of ops applied in order to
//! the graph the writer loaded, all or none, in one journaled commit with
//! one history commit.
//!
//! Each op is checked against the graph the earlier ops left, so a batch
//! adds a component before the decisions in it, and a refusal names the
//! first op at fault: the ops after it may build on it, so their faults
//! would describe a graph the batch never reaches. A decision an op
//! records, revises or merges into is held to the quality bar
//! (`store::quality`) in each field the op changes. Once every op holds,
//! the resulting graph is validated once under "no new errors", which
//! refuses what no op rule did. Only the files whose canonical text
//! changes are written.
//!
//! The batch is run in `run.rs`, its files committed in `files.rs`, and
//! what it did is `applied.rs`'s.

mod applied;
mod check;
mod component;
mod context;
mod decision;
mod edit;
mod files;
mod ops;
mod refusal;
mod repair;
mod run;
mod warning;

pub(crate) use applied::{Applied, Done, LeftRepair, Repaired};
pub(crate) use ops::{
    AddComponent, AnchorInput, Batch, Field, ListEdit, Merge, Op, OpKind, Record, Remove,
    RemoveComponent, Rename, Repair, RepairAnchors, Revise, ReviseComponent,
};
pub(crate) use refusal::Refusal;
#[cfg(test)]
pub(crate) use refusal::{NodeKind, OpAt, Rule};
pub(crate) use warning::{PatternNote, Warning, WarningKind};

#[cfg(test)]
mod tests;
