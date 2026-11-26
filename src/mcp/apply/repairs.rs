//! The `repair_anchors` op given no `repairs`: every repair the anchor
//! findings of `trurlic check` show, which the handler reads from the code
//! tree and hands the store as a map from each stale anchor to its repair;
//! and the part of the result that lists what was repaired and what was
//! left.
//!
//! Every repair is read before the write lock is taken, with the graph as
//! a read loads it: the tree walk and git hold no other writer, and the
//! index's cache can fill. A repair depends on its anchor alone, so the
//! store applies the map at the op's place in the batch to every decision
//! ever accepted that holds a stale anchor then, whatever the earlier ops,
//! or another write in between, did to the graph; a map with nothing to
//! repair repairs nothing and refuses nothing. The caller wrote none of
//! these repairs, so no verdict on one refuses the batch: the store, under
//! the lock, leaves the repairs of a decision the bar refuses or cannot
//! judge, with the rule as their cause.
//! The result lists the repairs the store made and the stale anchors left,
//! as many as the budget holds beside the rest of the result, and counts
//! the others: `trurlic check` lists every stale anchor.

use std::collections::BTreeMap;

use crate::code::Code;
use crate::lint::anchors::{Checked, Kind, Repair};
use crate::mcp::result::ToolError;
use crate::mcp::spec::output::{Count, Items, Nested, Str};
use crate::mcp::spec::{Output, Shown as Fields};
use crate::sources;

use crate::store::{Anchor, LeftRepair, Op, ProjectState, RepairAnchors, Store};

use super::result::Answer;

/// The stale anchors the tree shows no repair for, or whose repair a
/// pattern refuses, each with why.
pub(super) type Unrepaired = BTreeMap<Anchor, String>;

/// The code each repair the tree shows named when it was read, by its
/// stale anchor: what the write holds the repair to again under its lock.
pub(super) type Seen = BTreeMap<Anchor, Code>;

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

/// A stale anchor left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Left {
    pub(crate) decision: String,
    pub(crate) anchor: String,
    pub(crate) cause: String,
}

/// Fill each op of `ops` with no `repairs` with the repairs the tree
/// shows, read as `check` reads them; give the code each named, and the
/// stale anchors it shows no repair for, with why. Nothing is read when no
/// op is such.
pub(super) fn fill(store: &Store, ops: &mut [Op]) -> Result<(Seen, Unrepaired), ToolError> {
    // Every other op, a listed repair's included, is the caller's.
    let every = |op: &Op| matches!(op, Op::RepairAnchors(RepairAnchors::Every(_)));
    if !ops.iter().any(every) {
        return Ok((Seen::new(), Unrepaired::new()));
    }
    let state = store.load_state()?;

    let (repairs, seen, unrepaired) = shown(store, &state);

    for op in ops {
        if let Op::RepairAnchors(RepairAnchors::Every(map)) = op {
            // clone: each op with no `repairs` applies the whole map.
            map.clone_from(&repairs);
        }
    }
    Ok((seen, unrepaired))
}

/// The repair of each stale anchor `check` shows in `state` and the code it
/// names, and the cause of each stale anchor it shows none for.
fn shown(store: &Store, state: &ProjectState) -> (BTreeMap<Anchor, Anchor>, Seen, Unrepaired) {
    let (mut repairs, mut seen, mut unrepaired) = (BTreeMap::new(), Seen::new(), Unrepaired::new());
    let found = sources::checked_findings(store, &state.decisions);
    for finding in found.findings {
        let Kind::Missing(cause, Checked::Stale { repair, .. }) = finding.kind else {
            continue;
        };
        // clone: the map outlives the graph the finding borrows.
        let anchor = finding.anchor.clone();
        match repair {
            Some(Repair { to, code }) => {
                // clone: the code is held by the anchor the map keys too.
                seen.insert(anchor.clone(), code);
                repairs.insert(anchor, to);
            }
            None => {
                unrepaired.insert(anchor, cause.to_string());
            }
        }
    }
    (repairs, seen, unrepaired)
}

/// The stale anchors left, stated against `state`, the graph the write left
/// under its lock: each decision ever accepted that holds an anchor of
/// `unrepaired`, the tree showing no repair of it, or of `refused`, the
/// store not writing its repair, under its name there, by decision, then
/// anchor. An anchor whose repair the store left in several decisions
/// carries the cause of the first.
pub(super) fn left(
    state: &ProjectState,
    mut unrepaired: Unrepaired,
    refused: &[LeftRepair],
) -> Vec<Left> {
    for LeftRepair { repaired, rule } in refused {
        // clone: the map outlives the result it is read from.
        let stale = repaired.from.clone();
        let cause = || format!("its repair `{}` {rule}", repaired.to);
        unrepaired.entry(stale).or_insert_with(cause);
    }
    if unrepaired.is_empty() {
        return Vec::new();
    }
    let unrepaired = &unrepaired;
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
                    // clone: as above.
                    cause: unrepaired.get(anchor)?.clone(),
                })
            })
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
                 shows no repair for, or whose repair a `[quality]` pattern refuses, left as \
                 it was.";
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
        let cause = "Why it names nothing, or why its repair was not written.";
        fields.required("cause", cause, &Str, |item| &item.cause);
    }
}
