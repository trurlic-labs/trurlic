//! A batch run against the writer's graph ([`Writer::apply`]): the
//! batch's own faults first, then each op in order, refused at the first
//! at fault, then the check of the graph they leave, and the files that
//! take the store there.

use super::applied::{Applied, Done};
use super::check::Fault;
use super::context::Context;
use super::edit::Edit;
use super::ops::{Batch, Op};
use super::refusal::{OpAt, Rule, refused};
use super::{component, decision, repair, warning};
use crate::store::model::DecisionField;
use crate::store::quality::{Bar, Hit, PreparedBar, Scan};
use crate::store::write::{CommitMessage, Tool, Writer};
use crate::{Error, Result};

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
    pub(crate) fn apply(&mut self, batch: Batch, prepared: &PreparedBar) -> Result<Applied> {
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
