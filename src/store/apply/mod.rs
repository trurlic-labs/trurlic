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

mod check;
mod component;
mod decision;
mod edit;
mod ops;
mod refusal;
mod repair;
mod warning;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::store::format::Canonical;
use crate::store::write::PendingWrite;
use crate::store::write::{CommitMessage, Tool};

use crate::store::quality::{Bar, Hit, PreparedBar, Scan};

use crate::store::format::Store;
use crate::store::model::DecisionField;
use crate::store::model::NodeFile;
use crate::store::model::{Anchor, StatusName};
use crate::store::write::Writer;
use crate::{Error, Result};

use check::Fault;
use decision::Context;
use edit::{Before, Edit};

pub use ops::{
    AddComponent, AnchorInput, Batch, Field, ListEdit, Merge, Op, OpKind, Record, Remove,
    RemoveComponent, Rename, Repair, RepairAnchors, Revise, ReviseComponent,
};
#[cfg(test)]
pub(crate) use refusal::NodeKind;
pub use refusal::{OpAt, Refusal, Rule};
pub use warning::{PatternNote, Warning, WarningKind};

/// What a batch wrote: each op's outcome, in order, its warnings, and the
/// files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub ops: Vec<Done>,
    /// The warnings listed: each `warn` pattern warns at its first match
    /// per op, and the first [`MAX_WARNINGS_PER_OP`] of the origin's and of
    /// each op's are listed, in the order of the ops and the patterns.
    ///
    /// [`MAX_WARNINGS_PER_OP`]: crate::store::limits::MAX_WARNINGS_PER_OP
    pub warnings: Vec<Warning>,
    /// The origin's warnings not listed.
    pub more_origin_warnings: usize,
    /// The message of each pattern a listed warning names.
    pub patterns: Vec<PatternNote>,
    /// Files written, a rename's new file included.
    pub written: usize,
    /// Files removed, a rename's old file included.
    pub removed: usize,
    /// Each anchor the `repair_anchors` ops replaced, in the order they
    /// replaced them.
    pub repaired: Vec<Repaired>,
    /// Each repair of an op with no `repairs` that the bar did not admit,
    /// in the order they were judged.
    pub left: Vec<LeftRepair>,
}

/// An anchor a `repair_anchors` op replaced in a decision: `from` was held
/// and `to` is, `from` dropped when the decision held `to` already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repaired {
    pub decision: String,
    pub from: Anchor,
    pub to: Anchor,
}

/// A repair of an op with no `repairs` the store did not write, since the
/// bar refused it or could not judge it: the decision holds `from` still.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftRepair {
    pub repaired: Repaired,
    pub rule: Rule,
}

/// What one op did, named, never echoed: the node, the nodes it names
/// besides, the fields it changed, and how many other files it rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub op: OpKind,
    /// The node the op names first; a merge's survivor; the first decision
    /// a repair changed, empty when it changed none.
    pub name: String,
    /// A rename's new name.
    pub to: Option<String>,
    /// A merge's absorbed decision.
    pub absorbed: Option<String>,
    /// Where a remove pointed the edges that pointed at its decision.
    pub repoint_to: Option<String>,
    /// A revise's fields whose values changed, in the order of the file's
    /// keys.
    pub changed: Vec<Field>,
    /// The decision's status, when the op changed it.
    pub status: Option<StatusName>,
    /// Files of other nodes the op rewrote: dependents repointed, a
    /// component's decisions and connections renamed, the decisions a
    /// repair changed after its first.
    pub rewrote: usize,
    /// The op's warnings not listed in [`Applied::warnings`].
    pub more_warnings: usize,
}

impl Done {
    fn of(op: OpKind, name: String) -> Self {
        Self {
            op,
            name,
            to: None,
            absorbed: None,
            repoint_to: None,
            changed: Vec::new(),
            status: None,
            rewrote: 0,
            more_warnings: 0,
        }
    }
}

impl Writer<'_> {
    /// Apply `batch` to the graph and commit it in one write, or refuse it
    /// whole with [`Error::Refused`], writing nothing. Every decision it
    /// records is `proposed`, with the batch's origin and one instant.
    /// The writer's graph follows the commit; a refused batch, or a commit
    /// that fails before its commit point, leaves it as it was. The batch
    /// is held to `prepared`, the bar compiled before the lock, when the
    /// `[quality]` loaded under the lock is the one it was compiled from;
    /// else, the table edited in between, to that table compiled here. A
    /// `[quality]` pattern that does not compile refuses every batch with
    /// [`Error::QualityPattern`].
    pub fn apply(&mut self, batch: Batch, prepared: &PreparedBar) -> Result<Applied> {
        let message = commit_message(&batch)?;
        let compiled;
        let bar = match prepared.of(&self.state.project.quality) {
            Some(bar) => bar,
            None => {
                compiled = self.store.bar(&self.state.project.quality)?;
                &compiled
            }
        };
        let mut scan = bar.scan();
        let hits = origin_hits(&batch, bar, &mut scan)?;
        let mut context = Context::new(batch.origin.as_deref(), bar, scan, hits)?;
        let more_origin_warnings = warning::report(None, &mut context.hits, &mut context.warnings);
        let mut edit = Edit::new(&mut self.state);
        let (done, named) = match run(&mut edit, batch.ops, &mut context) {
            Ok(ran) => ran,
            Err(refusal) => {
                edit.undo();
                return Err(refusal);
            }
        };
        let (before, last_op) = edit.finish();
        let changed_by = |name: &str| last_op.get(name).and_then(|&op| named.get(op));
        let (written, removed) = self.land(before, &changed_by, &message)?;
        Ok(Applied {
            ops: done,
            repaired: context.repaired,
            left: context.left,
            patterns: warning::notes(&context.warnings, bar),
            warnings: context.warnings,
            more_origin_warnings,
            written,
            removed,
        })
    }

    /// Commit the files the batch changed from `before`, and count them
    /// written and removed; `changed_by` names the op that last changed a
    /// node, for a file a load could not read back. The writer's graph
    /// follows the files: past the commit point it is the batch's, before
    /// it the one `before` holds.
    fn land(
        &mut self,
        before: Before,
        changed_by: ChangedBy<'_, '_>,
        message: &CommitMessage,
    ) -> Result<(usize, usize)> {
        let committed = self
            .files(&before, changed_by)
            .and_then(|(writes, removes)| {
                self.commit(&writes, &removes, message)?;
                Ok((writes.len(), removes.len()))
            });
        match committed {
            Ok(counts) => {
                self.settle(&before);
                Ok(counts)
            }
            Err(e @ Error::WritePending { .. }) => {
                self.settle(&before);
                Err(e)
            }
            Err(e) => {
                before.restore(&mut self.state);
                Err(e)
            }
        }
    }

    /// The writes and removals that take the files from `before` to the
    /// writer's graph.
    fn files(
        &self,
        before: &Before,
        changed_by: ChangedBy<'_, '_>,
    ) -> Result<(Vec<PendingWrite>, Vec<PathBuf>)> {
        let mut files = Files {
            store: self.store,
            changed_by,
            writes: Vec::new(),
            removes: Vec::new(),
        };
        let state = &self.state;
        let (decision, component) = (Store::decision_path, Store::component_path);
        files.add(
            &before.decisions,
            &state.decisions,
            decision,
            NodeFile::Decision,
        )?;
        files.add(
            &before.components,
            &state.components,
            component,
            NodeFile::Component,
        )?;
        Ok((files.writes, files.removes))
    }

    /// Drop from the non-canonical list every file the batch wrote or
    /// removed: each is canonical, or gone.
    fn settle(&mut self, before: &Before) {
        let state = &mut self.state;
        state.non_canonical.retain(|file| match file {
            NodeFile::Project => true,
            NodeFile::Component(name) => !before.components.contains_key(name),
            NodeFile::Decision(name) => !before.decisions.contains_key(name),
        });
    }
}

/// The op that last changed a node, by the node's name.
type ChangedBy<'f, 'n> = &'f dyn Fn(&str) -> Option<&'n OpAt>;

/// The files a batch commits, gathered one node kind at a time.
struct Files<'f, 'n> {
    store: &'f Store,
    changed_by: ChangedBy<'f, 'n>,
    writes: Vec<PendingWrite>,
    removes: Vec<PathBuf>,
}

impl Files<'_, '_> {
    /// Add the writes and removals that take the nodes of one kind from
    /// `before` to `now`, each node's file at `path` and, in a refusal,
    /// named by `file`: a node whose value is as it was is not written. A
    /// canonical text over the cap a load reads refuses the batch, naming
    /// the op that last changed the node.
    fn add<T: Canonical + DeserializeOwned + PartialEq>(
        &mut self,
        before: &BTreeMap<String, Option<T>>,
        now: &BTreeMap<String, T>,
        path: fn(&Store, &str) -> PathBuf,
        file: fn(String) -> NodeFile,
    ) -> Result<()> {
        for (name, old) in before {
            match now.get(name) {
                Some(new) if old.as_ref() != Some(new) => {
                    let write = self.store.prepare_write(&path(self.store, name), new);
                    let write = write.map_err(|error| self.too_large(error, name, file))?;
                    self.writes.push(write);
                }
                Some(_) => {}
                None if old.is_some() => self.removes.push(path(self.store, name)),
                None => {}
            }
        }
        Ok(())
    }

    /// `error`; for a canonical text over the cap a load reads, the
    /// refusal naming the file `file` gives the node `name` and the op that
    /// last changed it.
    fn too_large(&self, error: Error, name: &str, file: fn(String) -> NodeFile) -> Error {
        match error {
            Error::CanonicalTooLarge { bytes, max, .. } => {
                // clone: the refusal owns the op it names.
                let op = (self.changed_by)(name).cloned();
                let file = file(name.to_owned());
                refused(op, None, Rule::FileTooLarge { file, bytes, max })
            }
            other => other,
        }
    }
}

/// The batch's own faults, before any op runs: no op, no origin where the
/// bar requires one, and an origin a pattern refuses; else the origin's
/// `warn` matches, judged over `scan` before any op's.
fn origin_hits(batch: &Batch, bar: &Bar, scan: &mut Scan) -> Result<Vec<(String, Hit)>> {
    if batch.ops.is_empty() {
        return Err(refused(None, Some("ops".to_owned()), Rule::EmptyBatch));
    }
    let field = || DecisionField::Origin.as_str().to_owned();
    let records = batch.ops.iter().any(|op| matches!(op, Op::Record(_)));
    let origin = batch.origin.as_deref();
    let hits = bar
        .origin_required(origin, records)
        .and_then(|()| bar.judge_origin(origin, scan))
        .map_err(|breach| refused(None, Some(field()), breach.broken.into()))?;
    Ok(hits.into_iter().map(|hit| (field(), hit)).collect())
}

/// Run each op in order, refusing at the first fault, then the check of
/// the resulting graph: each op's outcome, with its warnings listed in
/// `context`, and each op as a refusal names it.
fn run(
    edit: &mut Edit<'_>,
    ops: Vec<Op>,
    context: &mut Context<'_>,
) -> Result<(Vec<Done>, Vec<OpAt>)> {
    let mut done = Vec::with_capacity(ops.len());
    let mut named = Vec::with_capacity(ops.len());
    for (index, op) in ops.into_iter().enumerate() {
        edit.begin(index);
        let at = OpAt {
            index,
            kind: op.kind(),
            target: op.target().to_owned(),
        };
        match apply_op(edit, op, context) {
            Ok(mut outcome) => {
                let (hits, warnings) = (&mut context.hits, &mut context.warnings);
                outcome.more_warnings = warning::report(Some(index), hits, warnings);
                done.push(outcome);
            }
            Err(Fault {
                field,
                rule,
                target,
            }) => {
                let at = OpAt {
                    target: target.unwrap_or(at.target),
                    ..at
                };
                return Err(refused(Some(at), field, rule));
            }
        }
        named.push(at);
    }
    if let Some((index, issue)) = edit.first_introduced() {
        let at = named.into_iter().nth(index);
        return Err(refused(at, None, Rule::Issue(issue)));
    }
    Ok((done, named))
}

/// One op on the graph `edit` holds.
fn apply_op(
    edit: &mut Edit<'_>,
    op: Op,
    context: &mut Context<'_>,
) -> std::result::Result<Done, Fault> {
    match op {
        Op::Record(op) => decision::record(edit, op, context),
        Op::Revise(op) => decision::revise(edit, op, context),
        Op::Rename(op) => decision::rename(edit, op),
        Op::Merge(op) => decision::merge(edit, op, context),
        Op::Remove(op) => decision::remove(edit, op),
        Op::AddComponent(op) => component::add(edit, op),
        Op::ReviseComponent(op) => component::revise(edit, op),
        Op::RenameComponent(op) => component::rename(edit, op),
        Op::RemoveComponent(op) => component::remove(edit, op),
        Op::RepairAnchors(op) => repair::repair_anchors(edit, op, context),
    }
}

/// The batch's history message; an origin or a session that breaks the
/// one-line rule of a trailer refuses the batch, naming it.
fn commit_message(batch: &Batch) -> Result<CommitMessage> {
    // clone: the message and the recorded decisions each own the origin.
    let message = CommitMessage::new(Tool::Apply, batch.origin.clone(), batch.session.clone());
    message.map_err(|error| match error {
        Error::TrailerText { field, fault } => {
            refused(None, Some(field.to_string()), Rule::Text { field, fault })
        }
        other => other,
    })
}

fn refused(op: Option<OpAt>, field: Option<String>, rule: Rule) -> Error {
    Error::Refused(Box::new(Refusal { op, field, rule }))
}

#[cfg(test)]
mod tests;
