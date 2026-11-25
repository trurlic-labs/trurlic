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
mod patterns;
mod refusal;
mod warning;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::store::canonical::Canonical;
use crate::store::commit::PendingWrite;
use crate::store::history::{CommitMessage, Tool};
use crate::store::quality::{Bar, DecisionField};
use crate::store::schema::StatusName;
use crate::store::state::NodeFile;
use crate::store::time::Timestamp;
use crate::store::{Store, Writer};
use crate::{Error, Result};

use check::Fault;
use decision::Context;
use edit::{Before, Edit};
use patterns::Judged;

pub use ops::{
    AddComponent, AnchorInput, Batch, Field, ListEdit, Merge, Op, OpKind, Record, Remove,
    RemoveComponent, Rename, Revise, ReviseComponent,
};
pub use refusal::{NodeKind, OpAt, Refusal, Rule};
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
}

/// What one op did, named, never echoed: the node, the nodes it names
/// besides, the fields it changed, and how many other files it rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub op: OpKind,
    /// The node the op names first; a merge's survivor.
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
    /// component's decisions and connections renamed.
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
    /// that fails before its commit point, leaves it as it was. A
    /// `[quality]` pattern that does not compile refuses every batch with
    /// [`Error::QualityPattern`].
    pub fn apply(&mut self, batch: Batch) -> Result<Applied> {
        let message = commit_message(&batch)?;
        let bar = self.store.bar(&self.state.project.quality)?;
        check_batch(&batch, &bar)?;
        let mut context = Context {
            origin: batch.origin.as_deref(),
            created: Timestamp::now()?,
            bar: &bar,
            op: 0,
            pending: Vec::new(),
        };
        let mut edit = Edit::new(&mut self.state);
        let (done, named, judged) = match run(&mut edit, batch.ops, &mut context) {
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
            patterns: warning::notes(&judged.warnings, &bar),
            warnings: judged.warnings,
            more_origin_warnings: judged.more_origin,
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

/// The batch's own faults, before any op runs: no op, and no origin where
/// the bar requires one. Its patterns judge the origin with the ops'.
fn check_batch(batch: &Batch, bar: &Bar) -> Result<()> {
    if batch.ops.is_empty() {
        return Err(refused(None, Some("ops".to_owned()), Rule::EmptyBatch));
    }
    let records = batch.ops.iter().any(|op| matches!(op, Op::Record(_)));
    bar.origin_required(batch.origin.as_deref(), records)
        .map_err(|breach| {
            let field = Some(DecisionField::Origin.as_str().to_owned());
            refused(None, field, breach.broken.into())
        })
}

/// Each op's outcome, each op as a refusal names it, and what the patterns
/// found. The ops run in order to the first fault. Then the patterns judge
/// what the ops wrote (`patterns::judge`), and a refusal of theirs comes
/// first; then the op's fault; then the check of the resulting graph.
fn run(
    edit: &mut Edit<'_>,
    ops: Vec<Op>,
    context: &mut Context<'_>,
) -> Result<(Vec<Done>, Vec<OpAt>, Judged)> {
    let mut done = Vec::with_capacity(ops.len());
    let mut named = Vec::with_capacity(ops.len());
    let mut fault = None;
    for (index, op) in ops.into_iter().enumerate() {
        edit.begin(index);
        context.op = index;
        named.push(OpAt {
            index,
            kind: op.kind(),
            target: op.target().to_owned(),
        });
        match apply_op(edit, op, context) {
            Ok(outcome) => done.push(outcome),
            Err(at_fault) => {
                fault = Some(at_fault);
                break;
            }
        }
    }
    let judged = match patterns::judge(context.bar, context.origin, &context.pending) {
        Ok(judged) => judged,
        Err((op, Fault { field, rule })) => {
            let at = op.and_then(|index| named.into_iter().nth(index));
            return Err(refused(at, field, rule));
        }
    };
    if let Some(Fault { field, rule }) = fault {
        return Err(refused(named.pop(), field, rule));
    }
    if let Some((index, issue)) = edit.first_introduced() {
        let at = named.into_iter().nth(index);
        return Err(refused(at, None, Rule::Issue(issue)));
    }
    for (&index, &more) in &judged.more {
        if let Some(outcome) = done.get_mut(index) {
            outcome.more_warnings = more;
        }
    }
    Ok((done, named, judged))
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
