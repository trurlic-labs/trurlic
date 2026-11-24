//! `apply`, the one write of the graph: a batch of ops applied in order to
//! the graph the writer loaded, all or none, in one journaled commit with
//! one history commit.
//!
//! Each op is checked against the graph the earlier ops left, so a batch
//! adds a component before the decisions in it, and a refusal names the
//! first op at fault: the ops after it may build on it, so their faults
//! would describe a graph the batch never reaches. Once every op holds,
//! the resulting graph is validated once under "no new errors", which
//! refuses what no op rule did. Only the files whose canonical text
//! changes are written.

mod check;
mod component;
mod decision;
mod edit;
mod ops;
mod refusal;

use std::path::PathBuf;

use crate::store::Writer;
use crate::store::commit::PendingWrite;
use crate::store::history::{CommitMessage, Tool};
use crate::store::schema::StatusName;
use crate::store::state::NodeFile;
use crate::store::text::{TextField, fault};
use crate::store::time::Timestamp;
use crate::{Error, Result};

use check::Fault;
use decision::Recorded;
use edit::{Before, Edit};

pub use ops::{
    AddComponent, AnchorInput, Batch, Field, ListEdit, Merge, Op, OpKind, Record, Remove,
    RemoveComponent, Rename, Revise, ReviseComponent,
};
pub use refusal::{NodeKind, OpAt, Refusal, Rule};

/// What a batch wrote: each op's outcome, in order, and the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub ops: Vec<Done>,
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
        }
    }
}

impl Writer<'_> {
    /// Apply `batch` to the graph and commit it in one write, or refuse it
    /// whole with [`Error::Refused`], writing nothing. Every decision it
    /// records is `proposed`, with the batch's origin and one instant.
    /// The writer's graph follows the commit; a refused batch, or a commit
    /// that fails before its commit point, leaves it as it was.
    pub fn apply(&mut self, batch: Batch) -> Result<Applied> {
        let message = commit_message(&batch)?;
        if batch.ops.is_empty() {
            return Err(refused(None, Some("ops".to_owned()), Rule::EmptyBatch));
        }
        let recorded = Recorded {
            origin: batch.origin.as_deref(),
            created: Timestamp::now()?,
        };
        let mut edit = Edit::new(&mut self.state);
        let done = match run(&mut edit, batch.ops, &recorded) {
            Ok(done) => done,
            Err(refusal) => {
                edit.undo();
                return Err(refusal);
            }
        };
        let before = edit.finish();
        let files = self.files(&before);
        let committed = files.and_then(|(writes, removes)| {
            self.commit(&writes, &removes, &message)?;
            Ok((writes.len(), removes.len()))
        });
        match committed {
            Ok((written, removed)) => {
                self.settle(&before);
                Ok(Applied {
                    ops: done,
                    written,
                    removed,
                })
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
    /// writer's graph: a node whose value is as it was is not written.
    fn files(&self, before: &Before) -> Result<(Vec<PendingWrite>, Vec<PathBuf>)> {
        let (store, state) = (self.store, &self.state);
        let mut writes = Vec::new();
        let mut removes = Vec::new();
        for (name, old) in &before.decisions {
            let path = store.decision_path(name);
            match state.decisions.get(name) {
                Some(new) if old.as_ref() != Some(new) => {
                    writes.push(store.prepare_write(&path, new)?);
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
                    writes.push(store.prepare_write(&path, new)?);
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

/// Run each op in order, refusing at the first fault, then the check of
/// the resulting graph.
fn run(edit: &mut Edit<'_>, ops: Vec<Op>, recorded: &Recorded<'_>) -> Result<Vec<Done>> {
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
            Op::Record(op) => decision::record(edit, op, recorded),
            Op::Revise(op) => decision::revise(edit, op),
            Op::Rename(op) => decision::rename(edit, op),
            Op::Merge(op) => decision::merge(edit, op),
            Op::Remove(op) => decision::remove(edit, op),
            Op::AddComponent(op) => component::add(edit, op),
            Op::ReviseComponent(op) => component::revise(edit, op),
            Op::RenameComponent(op) => component::rename(edit, op),
            Op::RemoveComponent(op) => component::remove(edit, op),
        };
        match outcome {
            Ok(outcome) => done.push(outcome),
            Err(Fault { field, rule }) => return Err(refused(Some(at), field, rule)),
        }
        named.push(at);
    }
    if let Some((index, issue)) = edit.first_introduced() {
        let at = named.into_iter().nth(index);
        return Err(refused(at, None, Rule::Issue(issue)));
    }
    Ok(done)
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
