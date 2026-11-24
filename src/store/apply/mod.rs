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
mod warning;

use std::path::PathBuf;

use crate::store::Writer;
use crate::store::commit::PendingWrite;
use crate::store::history::{CommitMessage, Tool};
use crate::store::quality::{Bar, DecisionField, PatternMatch};
use crate::store::schema::StatusName;
use crate::store::state::NodeFile;
use crate::store::text::{TextField, fault};
use crate::store::time::Timestamp;
use crate::{Error, Result};

use check::Fault;
use decision::Context;
use edit::{Before, Edit};

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
        let mut context = Context {
            origin: batch.origin.as_deref(),
            created: Timestamp::now()?,
            bar: &bar,
            hits: origin_hits(&batch, &bar)?,
            warnings: Vec::new(),
        };
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
        let changed_by = |name: &str| last_op.get(name).and_then(|&op| named.get(op)).cloned();
        let (written, removed) = self.land(before, &changed_by, &message)?;
        Ok(Applied {
            ops: done,
            patterns: warning::notes(&context.warnings, &bar),
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
        changed_by: &dyn Fn(&str) -> Option<OpAt>,
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
    /// writer's graph: a node whose value is as it was is not written. A
    /// file whose canonical text a load would refuse refuses the batch,
    /// naming the op `changed_by` gives.
    fn files(
        &self,
        before: &Before,
        changed_by: &dyn Fn(&str) -> Option<OpAt>,
    ) -> Result<(Vec<PendingWrite>, Vec<PathBuf>)> {
        let (store, state) = (self.store, &self.state);
        let mut writes = Vec::new();
        let mut removes = Vec::new();
        for (name, old) in &before.decisions {
            let path = store.decision_path(name);
            match state.decisions.get(name) {
                Some(new) if old.as_ref() != Some(new) => {
                    let too_large = oversize(NodeFile::Decision, name, changed_by);
                    writes.push(store.prepare_write(&path, new).map_err(too_large)?);
                }
                Some(_) => {}
                None if old.is_some() => removes.push(path),
                None => {}
            }
        }
        for (name, old) in &before.components {
            let path = store.component_path(name);
            match state.components.get(name) {
                Some(new) if old.as_ref() != Some(new) => {
                    let too_large = oversize(NodeFile::Component, name, changed_by);
                    writes.push(store.prepare_write(&path, new).map_err(too_large)?);
                }
                Some(_) => {}
                None if old.is_some() => removes.push(path),
                None => {}
            }
        }
        Ok((writes, removes))
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

/// The refusal of a batch that leaves the canonical text of the node `name`
/// of the kind `file` over the cap a load reads, naming the op
/// `changed_by` gives; every other error as it is.
fn oversize<'a>(
    file: fn(String) -> NodeFile,
    name: &'a str,
    changed_by: &'a dyn Fn(&str) -> Option<OpAt>,
) -> impl FnOnce(Error) -> Error + 'a {
    move |error| match error {
        Error::CanonicalTooLarge { bytes, max, .. } => {
            let file = file(name.to_owned());
            refused(
                changed_by(name),
                None,
                Rule::FileTooLarge { file, bytes, max },
            )
        }
        other => other,
    }
}

/// The batch's own faults, before any op runs: no op, and an origin the
/// bar refuses; else the origin's warn matches.
fn origin_hits(batch: &Batch, bar: &Bar) -> Result<Vec<(String, PatternMatch)>> {
    if batch.ops.is_empty() {
        return Err(refused(None, Some("ops".to_owned()), Rule::EmptyBatch));
    }
    let records = batch.ops.iter().any(|op| matches!(op, Op::Record(_)));
    match bar.judge_origin(batch.origin.as_deref(), records) {
        Ok(hits) => Ok(hits
            .into_iter()
            .map(|hit| (DecisionField::Origin.as_str().to_owned(), hit.found))
            .collect()),
        Err(breach) => Err(refused(
            None,
            Some(DecisionField::Origin.as_str().to_owned()),
            breach.broken.into(),
        )),
    }
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
        let outcome = match op {
            Op::Record(op) => decision::record(edit, op, context),
            Op::Revise(op) => decision::revise(edit, op, context),
            Op::Rename(op) => decision::rename(edit, op),
            Op::Merge(op) => decision::merge(edit, op, context),
            Op::Remove(op) => decision::remove(edit, op),
            Op::AddComponent(op) => component::add(edit, op),
            Op::ReviseComponent(op) => component::revise(edit, op),
            Op::RenameComponent(op) => component::rename(edit, op),
            Op::RemoveComponent(op) => component::remove(edit, op),
        };
        match outcome {
            Ok(mut outcome) => {
                let (hits, warnings) = (&mut context.hits, &mut context.warnings);
                outcome.more_warnings = warning::report(Some(index), hits, warnings);
                done.push(outcome);
            }
            Err(Fault { field, rule }) => return Err(refused(Some(at), field, rule)),
        }
        named.push(at);
    }
    if let Some((index, issue)) = edit.first_introduced() {
        let at = named.into_iter().nth(index);
        return Err(refused(at, None, Rule::Issue(issue)));
    }
    Ok((done, named))
}

/// The batch's history message, its origin and session held to the
/// one-line rule of a trailer.
fn commit_message(batch: &Batch) -> Result<CommitMessage> {
    for (field, value) in [
        (TextField::Origin, &batch.origin),
        (TextField::Session, &batch.session),
    ] {
        if let Some(fault) = value.as_deref().and_then(|text| fault(field, text)) {
            return Err(refused(
                None,
                Some(field.to_string()),
                Rule::Text { field, fault },
            ));
        }
    }
    // clone: the message and the recorded decisions each own the origin.
    CommitMessage::new(Tool::Apply, batch.origin.clone(), batch.session.clone())
}

fn refused(op: Option<OpAt>, field: Option<String>, rule: Rule) -> Error {
    Error::Refused(Box::new(Refusal { op, field, rule }))
}

#[cfg(test)]
mod tests;
