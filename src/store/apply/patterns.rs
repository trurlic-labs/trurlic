//! The `[quality]` patterns of a batch, judged once every op has run.
//! Each op holds what it writes to the text rules and the limits as it
//! runs, and keeps it here. Then the work the patterns would do over the
//! batch's origin and every kept decision is charged in op order against
//! [`MAX_PATTERN_WORK`]; a batch past it is refused before any pattern
//! runs. Else the patterns judge the origin, then each kept decision in op
//! order. A pattern's refusal at an op comes before any fault found later,
//! by a later op or by the same op's later checks. That is the order the
//! patterns would give had they run with the op.

use std::collections::BTreeMap;

use super::check::{Fault, Sources};
use super::refusal::Rule;
use super::warning::{self, Warning};
use crate::store::limits::MAX_PATTERN_WORK;
use crate::store::quality::{Bar, DecisionField, Hit};
use crate::store::schema::Decision;

/// A decision an op wrote, held to the patterns in the fields of `held`.
#[derive(Debug)]
pub(super) struct Pending {
    pub(super) op: usize,
    pub(super) decision: Decision,
    pub(super) held: Vec<DecisionField>,
    pub(super) sources: Sources,
}

/// What the patterns found in a batch that passes them: the warnings each
/// op and the origin list, and the count of those each leaves unlisted.
#[derive(Debug, Default)]
pub(super) struct Judged {
    pub(super) warnings: Vec<Warning>,
    /// By op index, the op's warnings not listed.
    pub(super) more: BTreeMap<usize, usize>,
    pub(super) more_origin: usize,
}

/// A refusal of the patterns: the op at fault, `None` for the origin.
pub(super) type Refused = (Option<usize>, Fault);

/// The batch's `origin` and each of `pending` against the patterns of
/// `bar`, once their work is charged.
pub(super) fn judge(
    bar: &Bar,
    origin: Option<&str>,
    pending: &[Pending],
) -> Result<Judged, Refused> {
    if bar.pattern_count() == 0 {
        return Ok(Judged::default());
    }
    check_work(bar, origin, pending)?;
    let mut judged = Judged::default();
    let mut hits = origin_hits(bar, origin)?;
    judged.more_origin = warning::report(None, &mut hits, &mut judged.warnings);
    for kept in pending.chunk_by(|one, other| one.op == other.op) {
        let mut hits = Vec::new();
        for one in kept {
            hits.extend(pattern_hits(bar, one)?);
        }
        if let Some(first) = kept.first() {
            let more = warning::report(Some(first.op), &mut hits, &mut judged.warnings);
            judged.more.insert(first.op, more);
        }
    }
    Ok(judged)
}

/// The `warn` matches of the patterns of `bar` in the batch's `origin`,
/// or the refusal of the first `refuse` one.
fn origin_hits(bar: &Bar, origin: Option<&str>) -> Result<Vec<(String, Hit)>, Refused> {
    let field = || DecisionField::Origin.as_str().to_owned();
    match bar.judge_origin(origin) {
        Ok(hits) => Ok(hits.into_iter().map(|hit| (field(), hit)).collect()),
        Err(breach) => {
            let rule = breach.broken.into();
            Err((
                None,
                Fault {
                    field: Some(field()),
                    rule,
                },
            ))
        }
    }
}

/// The `warn` matches of the patterns of `bar` in what `kept` holds, each
/// at the argument it is in, or the refusal of the first `refuse` one.
fn pattern_hits(bar: &Bar, kept: &Pending) -> Result<Vec<(String, Hit)>, Refused> {
    let Pending {
        op,
        decision,
        held,
        sources,
    } = kept;
    match bar.judge_patterns(decision, held) {
        Ok(found) => Ok(found
            .into_iter()
            .map(|(place, hit)| (sources.path(decision, place), hit))
            .collect()),
        Err(breach) => {
            let field = Some(sources.path(decision, breach.place));
            let rule = breach.broken.into();
            Err((Some(*op), Fault { field, rule }))
        }
    }
}

/// The work the patterns of `bar` would do over `origin`, then each of
/// `pending`, summed in that order: refused at the op, or the origin,
/// where the sum passes [`MAX_PATTERN_WORK`]; one whose own work passes it
/// names the costliest pattern for it.
fn check_work(bar: &Bar, origin: Option<&str>, pending: &[Pending]) -> Result<(), Refused> {
    let origin_work = (None, bar.origin_work(origin));
    let ops = pending
        .iter()
        .map(|kept| (Some(kept.op), bar.judge_work(&kept.decision, &kept.held)));
    let mut spent = 0usize;
    for (op, costs) in std::iter::once(origin_work).chain(ops) {
        let own = costs
            .iter()
            .fold(0usize, |sum, &cost| sum.saturating_add(cost));
        spent = spent.saturating_add(own);
        if spent <= MAX_PATTERN_WORK {
            continue;
        }
        let alone = own > MAX_PATTERN_WORK;
        let costliest = costs
            .iter()
            .enumerate()
            .max_by_key(|&(index, &cost)| (cost, std::cmp::Reverse(index)))
            .map(|(index, _)| index)
            .filter(|_| alone);
        let rule = Rule::PatternWork {
            work: if alone { own } else { spent },
            bound: MAX_PATTERN_WORK,
            costliest,
        };
        let field = op
            .is_none()
            .then(|| DecisionField::Origin.as_str().to_owned());
        return Err((op, Fault { field, rule }));
    }
    Ok(())
}
