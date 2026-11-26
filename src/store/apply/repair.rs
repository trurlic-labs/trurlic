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

use std::collections::{BTreeMap, BTreeSet};

use super::check::{Fault, Sources};
use super::decision::Context;
use super::edit::Edit;
use super::ops::{AnchorInput, Field, OpKind, Repair, RepairAnchors};
use super::refusal::{NodeKind, Rule};
use super::{Done, LeftRepair, Repaired};
use crate::store::quality::{Bar, Breach, DecisionField, Hit, Place, Scan, texts};
use crate::store::schema::{Anchor, Decision};
use crate::store::state::ProjectState;

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
    let changes = match op {
        RepairAnchors::Listed(repairs) => {
            let changes = listed(edit, repairs)?;
            judge_listed(edit, &changes, context)?;
            changes
        }
        RepairAnchors::Every(map) => admitted(edit, &map, context),
    };
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

/// The decisions ever accepted that hold an anchor `map` holds a repair
/// of, by name; a decision never accepted plans its anchors, which no
/// repair moves.
fn holders(state: &ProjectState, map: &BTreeMap<Anchor, Anchor>) -> Vec<String> {
    let holds = |decision: &Decision| decision.anchors.iter().any(|a| map.contains_key(a));
    state
        .decisions
        .iter()
        .filter(|(_, decision)| decision.status.ever_accepted() && holds(decision))
        // clone: the decision is changed past this borrow of the graph.
        .map(|(name, _)| name.clone())
        .collect()
}

/// Each anchor of `decision`, the one named `name`, that `map` holds a
/// repair of, replaced by it, in the order of the anchors.
fn replaced(decision: &mut Decision, name: &str, map: &BTreeMap<Anchor, Anchor>) -> Vec<Change> {
    let stale = decision
        .anchors
        .iter()
        .filter(|anchor| map.contains_key(*anchor));
    // clone: the anchors are replaced in the set they are read from.
    let stale: Vec<Anchor> = stale.cloned().collect();
    let mut changes = Vec::with_capacity(stale.len());
    for from in stale {
        let Some(to) = map.get(&from) else { continue };
        decision.anchors.remove(&from);
        // clone: one repair of the map serves every decision holding it.
        let gained = decision.anchors.insert(to.clone());
        let repaired = Repaired {
            decision: name.to_owned(),
            from,
            // clone: the decision holds the repair, and the result names it.
            to: to.clone(),
        };
        changes.push(Change { repaired, gained });
    }
    changes
}

/// The anchors `changes` brought `decision` that it did not hold before,
/// together, held to the patterns over `scan`: their `warn` matches, each
/// with where it is, or what stopped them.
fn judged(
    bar: &Bar,
    decision: &Decision,
    changes: &[&Change],
    scan: &mut Scan,
) -> Result<Vec<(Place, Hit)>, Breach> {
    let gained: BTreeSet<String> = changes
        .iter()
        .filter(|change| change.gained)
        .map(|change| change.repaired.to.to_string())
        .collect();
    let mut read = texts(decision, DecisionField::Anchors);
    read.retain(|(_, text)| gained.contains(text.as_ref()));
    bar.judge_patterns(&read, scan)
}

/// Hold each decision `changes` changed to the patterns in the anchors it
/// gained, by name, its warnings joining the op's. An anchor a listed
/// repair brought is named by its index in the op's list of repairs; a
/// refusal names the decision at fault.
fn judge_listed(
    edit: &Edit<'_>,
    changes: &[Change],
    context: &mut Context<'_>,
) -> Result<(), Fault> {
    let mut sources = Sources::default();
    let given = changes.iter().map(|c| c.repaired.to.to_string()).collect();
    sources.add(DecisionField::Anchors, Field::Repairs, given);
    let names: BTreeSet<&str> = changes
        .iter()
        .map(|change| change.repaired.decision.as_str())
        .collect();
    for name in names {
        let Some(decision) = edit.state().decisions.get(name) else {
            continue;
        };
        let own: Vec<&Change> = changes
            .iter()
            .filter(|change| change.repaired.decision == name)
            .collect();
        let hits = judged(context.bar, decision, &own, &mut context.scan);
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

/// The repairs of the "every repair" `map` the patterns admit, judged by
/// the anchor they bring: in each decision holding a stale anchor, its
/// anchors replaced, each repair it gained is held to the patterns. The
/// caller wrote none of them, so none refuses the batch: a repair the
/// patterns refuse or cannot judge (a `refuse` match, a give-up, the cache
/// total, the bytes bound) is undone in every change that brought it, each
/// stale anchor it replaced put back and left with the rule.
fn admitted(
    edit: &mut Edit<'_>,
    map: &BTreeMap<Anchor, Anchor>,
    context: &mut Context<'_>,
) -> Vec<Change> {
    let mut kept = Vec::new();
    for name in holders(edit.state(), map) {
        let Some(decision) = edit.decision_mut(&name) else {
            continue;
        };
        let changes = replaced(decision, &name, map);
        let refused = refused(decision, &changes, context);
        for change in changes {
            let Some(rule) = refused.get(&change.repaired.to) else {
                kept.push(change);
                continue;
            };
            if change.gained {
                decision.anchors.remove(&change.repaired.to);
            }
            // clone: the decision and the repair left both name it.
            decision.anchors.insert(change.repaired.from.clone());
            context.left.push(LeftRepair {
                repaired: change.repaired,
                // clone: each repair left names the rule.
                rule: rule.clone(),
            });
        }
    }
    kept
}

/// The repairs of `changes` the patterns refuse or cannot judge in
/// `decision`, each with the rule: each repair it gained judged on its own
/// over the batch's scan, the warnings of the others joining the op's.
/// What judging one read and built stays charged, as every search's does
/// ([`Scan`]), and a cache that gave up on it is emptied.
fn refused(
    decision: &Decision,
    changes: &[Change],
    context: &mut Context<'_>,
) -> BTreeMap<Anchor, Rule> {
    let repairs: BTreeSet<&Anchor> = changes.iter().map(|change| &change.repaired.to).collect();
    let mut refused = BTreeMap::new();
    for to in repairs {
        let brought: Vec<&Change> = changes
            .iter()
            .filter(|change| change.repaired.to == *to)
            .collect();
        match judged(context.bar, decision, &brought, &mut context.scan) {
            Ok(hits) => {
                let field = || DecisionField::Anchors.as_str().to_owned();
                let located = hits.into_iter().map(|(_, hit)| (field(), hit));
                context.hits.extend(located);
            }
            Err(breach) => {
                // clone: the rule is read by every change that brought it.
                refused.insert(to.clone(), breach.broken.into());
            }
        }
    }
    refused
}

/// `input` as an anchor, or the rule it breaks.
fn anchor(input: AnchorInput) -> Result<Anchor, Rule> {
    Anchor::new(input.file, input.symbol).map_err(Rule::Anchor)
}
