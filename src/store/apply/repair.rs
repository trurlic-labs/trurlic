//! The `repair_anchors` op: stale anchors replaced in their decisions,
//! checked against the graph the earlier ops left. The store reads no code
//! and runs no git: which anchor repairs which is the caller's, given as a
//! list of repairs or as a map from each stale anchor to its repair, and
//! the store holds each repair to the rules every op is held to.
//!
//! A repair replaces an anchor, or drops it when the decision holds the new
//! one already, so a decision's count of anchors never grows: the limits on
//! the whole list are not judged, since a field the op leaves as it was
//! keeps what it held. The patterns of the bar judge the anchors each
//! decision gained, a repair to an anchor it held already gaining none; a
//! refusal names that decision, and the repair when one was listed. A
//! change of anchors leaves the status alone.

use std::collections::BTreeMap;

use super::check::{Fault, Sources};
use super::decision::Context;
use super::edit::Edit;
use super::ops::{AnchorInput, Field, OpKind, Repair, RepairAnchors};
use super::refusal::{NodeKind, Rule};
use super::{Done, Repaired};
use crate::store::quality::{DecisionField, texts};
use crate::store::schema::Anchor;

/// One anchor replaced, and whether its decision gained the new anchor,
/// which it did not hold before.
struct Change {
    repaired: Repaired,
    gained: bool,
}

/// Replace the anchors `op` names, each decision changed held to the
/// patterns in the anchors it gained.
pub(super) fn repair_anchors(
    edit: &mut Edit<'_>,
    op: RepairAnchors,
    context: &mut Context<'_>,
) -> Result<Done, Fault> {
    let (changes, given) = match op {
        RepairAnchors::Listed(repairs) => {
            let changes = listed(edit, repairs)?;
            let given = changes.iter().map(|c| c.repaired.to.to_string()).collect();
            (changes, given)
        }
        RepairAnchors::Every(map) => (every(edit, &map), Vec::new()),
    };
    judge_gained(edit, &changes, given, context)?;
    let mut decisions: Vec<&str> = changes
        .iter()
        .map(|change| change.repaired.decision.as_str())
        .collect();
    decisions.sort_unstable();
    decisions.dedup();
    let rewrote = decisions.len().saturating_sub(1);
    // clone: the result names the decision the batch's repairs list too.
    let first = changes
        .first()
        .map(|change| change.repaired.decision.clone());
    context
        .repaired
        .extend(changes.into_iter().map(|change| change.repaired));
    let changed = match first {
        Some(_) => vec![Field::Anchors],
        None => Vec::new(),
    };
    Ok(Done {
        changed,
        rewrote,
        ..Done::of(OpKind::RepairAnchors, first.unwrap_or_default())
    })
}

/// Each listed repair in order: the decision exists and holds `from`, `to`
/// is an anchor and differs from `from`. An empty list is refused.
fn listed(edit: &mut Edit<'_>, repairs: Vec<Repair>) -> Result<Vec<Change>, Fault> {
    if repairs.is_empty() {
        return Err(Fault::at(Field::Repairs, Rule::NoRepair));
    }
    repairs
        .into_iter()
        .enumerate()
        .map(|(index, repair)| replace(edit, index, repair))
        .collect()
}

/// Replace `repair.from` by `repair.to` in its decision, the repair at
/// `index` of the op.
fn replace(edit: &mut Edit<'_>, index: usize, repair: Repair) -> Result<Change, Fault> {
    let at = |part: &str, rule| {
        Fault::named(format!("{}[{index}].{part}", Field::Repairs.as_str()), rule)
    };
    let Repair { decision, from, to } = repair;
    let from = anchor(from).map_err(|rule| at("from", rule))?;
    let to = anchor(to).map_err(|rule| at("to", rule))?;
    if to == from {
        return Err(at("to", Rule::NoChange));
    }
    let Some(held) = edit.decision_mut(&decision) else {
        let kind = NodeKind::Decision;
        let rule = Rule::NotFound {
            name: decision,
            kind,
        };
        return Err(at("decision", rule));
    };
    if !held.anchors.remove(&from) {
        let element = from.to_string();
        return Err(at("from", Rule::NotHeld { element }));
    }
    // clone: the decision holds the anchor, and the result names it.
    let gained = held.anchors.insert(to.clone());
    Ok(Change {
        repaired: Repaired { decision, from, to },
        gained,
    })
}

/// Each anchor `map` holds a repair of, replaced in every decision ever
/// accepted that holds it now, by decision and anchor; a decision never
/// accepted plans its anchors, which no repair moves.
fn every(edit: &mut Edit<'_>, map: &BTreeMap<Anchor, Anchor>) -> Vec<Change> {
    let holders: Vec<(String, Vec<Anchor>)> = edit
        .state()
        .decisions
        .iter()
        .filter(|(_, decision)| decision.status.ever_accepted())
        .filter_map(|(name, decision)| {
            // clone: the anchors are replaced in the decision they are read from.
            let held: Vec<Anchor> = decision
                .anchors
                .iter()
                .filter(|anchor| map.contains_key(*anchor))
                .cloned()
                .collect();
            // clone: the change names the decision past this borrow.
            (!held.is_empty()).then(|| (name.clone(), held))
        })
        .collect();
    let mut changes = Vec::new();
    for (name, held) in holders {
        let Some(decision) = edit.decision_mut(&name) else {
            continue;
        };
        for from in held {
            let Some(to) = map.get(&from) else { continue };
            decision.anchors.remove(&from);
            // clone: one repair of the map serves every decision holding it.
            let gained = decision.anchors.insert(to.clone());
            let repaired = Repaired {
                // clone: each change names its decision.
                decision: name.clone(),
                from,
                // clone: the decision holds the repair, and the result names it.
                to: to.clone(),
            };
            changes.push(Change { repaired, gained });
        }
    }
    changes
}

/// Hold each decision `changes` changed to the patterns in the anchors it
/// gained, by name, its warnings joining the op's. An anchor a listed
/// repair brought is named by its index in `given`, the caller's list of
/// new anchors; a refusal names the decision at fault.
fn judge_gained(
    edit: &Edit<'_>,
    changes: &[Change],
    given: Vec<String>,
    context: &mut Context<'_>,
) -> Result<(), Fault> {
    let mut gained: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for change in changes.iter().filter(|change| change.gained) {
        let Repaired { decision, to, .. } = &change.repaired;
        gained
            .entry(decision.as_str())
            .or_default()
            .push(to.to_string());
    }
    let mut sources = Sources::default();
    sources.add(DecisionField::Anchors, Field::Repairs, given);
    let changed = edit
        .state()
        .decisions
        .iter()
        .filter_map(|(name, decision)| {
            let anchors = gained.get(name.as_str())?;
            Some((name, decision, anchors))
        });
    for (name, decision, anchors) in changed {
        let mut read = texts(decision, DecisionField::Anchors);
        read.retain(|(_, text)| anchors.iter().any(|anchor| anchor.as_str() == text));
        let hits = context.bar.judge_patterns(&read, &mut context.scan);
        let hits = hits.map_err(|breach| Fault {
            target: Some(name.to_owned()),
            ..sources.fault(decision, breach)
        })?;
        let located = hits
            .into_iter()
            .map(|(place, hit)| (sources.path(decision, place), hit));
        context.hits.extend(located);
    }
    Ok(())
}

/// `input` as an anchor, or the rule it breaks.
fn anchor(input: AnchorInput) -> Result<Anchor, Rule> {
    Anchor::new(input.file, input.symbol).map_err(Rule::Anchor)
}
