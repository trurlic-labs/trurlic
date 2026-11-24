//! The `repair_anchors` op as a call gives it: the repairs it lists, or,
//! with none, every repair the anchor findings of `trurlic check` show,
//! which the handler reads from the code tree and hands the store as a map
//! from each stale anchor to its repair; and the part of the result that
//! lists what was repaired and what was left.
//!
//! Every repair is read before the write lock is taken, with the graph as
//! a read loads it: the tree walk and git hold no other writer, and the
//! index's cache can fill. A repair depends on its anchor alone, so the
//! store applies the map at the op's place in the batch to every decision
//! ever accepted that holds a stale anchor then, whatever the earlier ops,
//! or another write in between, did to the graph; a map with nothing to
//! repair repairs nothing and refuses nothing. The result lists the repairs
//! the store made and the stale anchors the tree showed no repair for, as
//! many as the budget holds beside the rest of the result, and counts the
//! others: `trurlic check` lists every stale anchor.

use std::collections::BTreeMap;

use crate::lint::anchors::{Checked, Kind};
use crate::mcp::result::ToolError;
use crate::mcp::spec::output::{Count, Items, Nested, Str};
use crate::mcp::spec::{Output, Shown as Fields};
use crate::sources;
use crate::store::{Anchor, Op, ProjectState, RepairAnchors, Store};

use super::result::Answer;

/// An op as a call gives it: an op of the store, or every repair the
/// tree shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Requested {
    /// Boxed: an op is many times the size of the other variant.
    Op(Box<Op>),
    EveryRepair,
}

impl Requested {
    pub(super) fn op(op: Op) -> Self {
        Self::Op(Box::new(op))
    }
}

/// What the tree shows for an op asking for every repair, read before the
/// write: the repair of each stale anchor that has one, and the cause of
/// each that has none. Empty when no op asks.
#[derive(Debug, Default)]
pub(super) struct Shown {
    repairs: BTreeMap<Anchor, Anchor>,
    unrepaired: BTreeMap<Anchor, String>,
}

/// A repair the batch wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repaired {
    pub(crate) decision: String,
    pub(crate) anchor: String,
    pub(crate) repair: String,
}

/// A repair as the result names it: the anchors as `file::symbol`.
impl From<crate::store::Repaired> for Repaired {
    fn from(repaired: crate::store::Repaired) -> Self {
        Self {
            decision: repaired.decision,
            anchor: repaired.from.to_string(),
            repair: repaired.to.to_string(),
        }
    }
}

/// A stale anchor the tree shows no repair for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Left {
    pub(crate) decision: String,
    pub(crate) anchor: String,
    pub(crate) cause: String,
}

/// What the tree shows when an op of `requested` asks for every repair,
/// read as `check` reads it; nothing when none asks.
pub(super) fn shown(store: &Store, requested: &[Requested]) -> Result<Shown, ToolError> {
    let mut shown = Shown::default();
    if !requested.contains(&Requested::EveryRepair) {
        return Ok(shown);
    }
    let state = store.load_state()?;
    let found = sources::checked_findings(store, &state.decisions);
    for finding in found.findings {
        let Kind::Missing(cause, Checked::Stale { repair, .. }) = finding.kind else {
            continue;
        };
        // clone: the map outlives the graph the finding borrows.
        let anchor = finding.anchor.clone();
        match repair {
            Some(repair) => {
                shown.repairs.insert(anchor, repair);
            }
            None => {
                shown.unrepaired.insert(anchor, cause.to_string());
            }
        }
    }
    Ok(shown)
}

/// The stale anchors left without a repair, stated against `state`, the
/// graph the write left under its lock: each decision ever accepted that
/// holds an anchor `shown` has no repair for, under its name there, by
/// decision, then anchor.
pub(super) fn left(state: &ProjectState, shown: &Shown) -> Vec<Left> {
    if shown.unrepaired.is_empty() {
        return Vec::new();
    }
    state
        .decisions
        .iter()
        .filter(|(_, decision)| decision.status.ever_accepted())
        .flat_map(|(name, decision)| {
            decision.anchors.iter().filter_map(move |anchor| {
                Some(Left {
                    // clone: the result outlives the graph it names.
                    decision: name.clone(),
                    anchor: anchor.to_string(),
                    cause: shown.unrepaired.get(anchor)?.clone(),
                })
            })
        })
        .collect()
}

/// The store's ops of `requested`, each asking for every repair given the
/// map `shown` holds.
pub(super) fn ops(requested: Vec<Requested>, shown: &Shown) -> Vec<Op> {
    requested
        .into_iter()
        .map(|requested| match requested {
            Requested::Op(op) => *op,
            // clone: each op asking for every repair applies the whole map.
            Requested::EveryRepair => {
                Op::RepairAnchors(RepairAnchors::Every(shown.repairs.clone()))
            }
        })
        .collect()
}

/// The result's fields of the repairs: the lists and their counts.
pub(super) fn declare(fields: &mut Fields<'_, Answer>) {
    let repaired = Items {
        item: Nested::<Repaired>::new(),
    };
    let about = "Each anchor a `repair_anchors` op replaced, with what it replaced it by.";
    fields.optional("repaired", about, &repaired, |answer| {
        (!answer.repaired.is_empty()).then_some(&answer.repaired)
    });
    let more = "Repairs written and not listed: `trurlic check` shows the anchors as they are.";
    fields.optional("more_repaired", more, &Count, |answer| {
        (answer.more_repaired > 0).then_some(&answer.more_repaired)
    });
    let left = Items {
        item: Nested::<Left>::new(),
    };
    let about = "For a `repair_anchors` op with no `repairs`: each stale anchor the tree \
                 shows no repair for, left as it was.";
    fields.optional("left", about, &left, |answer| {
        (!answer.left.is_empty()).then_some(&answer.left)
    });
    let more = "Stale anchors left and not listed: `trurlic check` lists every one.";
    fields.optional("more_left", more, &Count, |answer| {
        (answer.more_left > 0).then_some(&answer.more_left)
    });
}

impl Output for Repaired {
    fn declare(fields: &mut Fields<'_, Self>) {
        fields.required("decision", "The decision.", &Str, |item| &item.decision);
        let anchor = "The anchor replaced, `file` or `file::symbol`.";
        fields.required("anchor", anchor, &Str, |item| &item.anchor);
        let repair = "The anchor written instead.";
        fields.required("repair", repair, &Str, |item| &item.repair);
    }
}

impl Output for Left {
    fn declare(fields: &mut Fields<'_, Self>) {
        fields.required("decision", "The decision.", &Str, |item| &item.decision);
        let anchor = "The stale anchor, `file` or `file::symbol`.";
        fields.required("anchor", anchor, &Str, |item| &item.anchor);
        let cause = "Why it names nothing.";
        fields.required("cause", cause, &Str, |item| &item.cause);
    }
}
