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
//! these repairs, so none may refuse the batch: a repair a `refuse` pattern
//! of `[quality]` matches on the anchors field, the one the store judges a
//! repair on, is taken out of the map and left, with the pattern as its
//! cause. The result lists the repairs the store made and the stale
//! anchors left, as many as the budget holds beside the rest of the
//! result, and counts the others: `trurlic check` lists every stale anchor.

use std::collections::{BTreeMap, BTreeSet};

use crate::lint::anchors::{Checked, Kind};
use crate::mcp::result::ToolError;
use crate::mcp::spec::output::{Count, Items, Nested, Str};
use crate::mcp::spec::{Output, Shown as Fields};
use crate::sources;
use crate::store::limits::MAX_PATTERN_WORK;
use crate::store::{Anchor, Bar, DecisionField, Op, ProjectState, RepairAnchors, Store};

use super::result::Answer;

/// The stale anchors the tree shows no repair for, or whose repair a
/// pattern refuses, each with why.
pub(super) type Unrepaired = BTreeMap<Anchor, String>;

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

/// Fill each op of `ops` with no `repairs` with the repairs the tree shows,
/// read as `check` reads them, less those a `refuse` pattern matches; give
/// the stale anchors left without a repair, with why. Nothing is read when
/// no op is such.
pub(super) fn fill(store: &Store, ops: &mut [Op]) -> Result<Unrepaired, ToolError> {
    // Every other op, a listed repair's included, is the caller's.
    let every = |op: &Op| matches!(op, Op::RepairAnchors(RepairAnchors::Every(_)));
    if !ops.iter().any(every) {
        return Ok(Unrepaired::new());
    }
    let state = store.load_state()?;
    let bar = store.bar(&state.project.quality)?;
    let (mut repairs, mut unrepaired) = shown(store, &state);
    sift(&bar, &state, &mut repairs, &mut unrepaired);
    for op in ops {
        if let Op::RepairAnchors(RepairAnchors::Every(map)) = op {
            // clone: each op with no `repairs` applies the whole map.
            map.clone_from(&repairs);
        }
    }
    Ok(unrepaired)
}

/// The repair of each stale anchor `check` shows in `state`, and the cause
/// of each stale anchor it shows none for.
fn shown(store: &Store, state: &ProjectState) -> (BTreeMap<Anchor, Anchor>, Unrepaired) {
    let (mut repairs, mut unrepaired) = (BTreeMap::new(), Unrepaired::new());
    let found = sources::checked_findings(store, &state.decisions);
    for finding in found.findings {
        let Kind::Missing(cause, Checked::Stale { repair, .. }) = finding.kind else {
            continue;
        };
        // clone: the map outlives the graph the finding borrows.
        let anchor = finding.anchor.clone();
        match repair {
            Some(repair) => {
                repairs.insert(anchor, repair);
            }
            None => {
                unrepaired.insert(anchor, cause.to_string());
            }
        }
    }
    (repairs, unrepaired)
}

/// Move out of `repairs` to `unrepaired` each repair a `refuse` pattern of
/// `bar` matches on the anchors field, which holds it alone, as the store
/// judges the anchors a repair brings. The work is charged first: once it
/// passes [`MAX_PATTERN_WORK`] the rest stay for the write, which judges
/// every repair it makes and refuses a batch that costs too much.
fn sift(
    bar: &Bar,
    state: &ProjectState,
    repairs: &mut BTreeMap<Anchor, Anchor>,
    unrepaired: &mut Unrepaired,
) {
    let Some(held) = state.decisions.values().next() else {
        return;
    };
    if bar.pattern_count() == 0 {
        return;
    }
    // clone: the probe holds one repair's anchor at a time.
    let mut probe = held.clone();
    let field = [DecisionField::Anchors];
    let mut spent = 0usize;
    repairs.retain(|from, to| {
        // clone: as above.
        probe.anchors = BTreeSet::from([to.clone()]);
        let work = bar.judge_work(&probe, &field).into_iter();
        spent = spent.saturating_add(work.fold(0, usize::saturating_add));
        if spent > MAX_PATTERN_WORK {
            return true;
        }
        let Err(breach) = bar.judge_patterns(&probe, &field) else {
            return true;
        };
        let cause = format!("its repair `{to}` {}", breach.broken);
        // clone: the map left outlives the one sifted.
        unrepaired.insert(from.clone(), cause);
        false
    });
}

/// The stale anchors left without a repair, stated against `state`, the
/// graph the write left under its lock: each decision ever accepted that
/// holds an anchor of `unrepaired`, under its name there, by decision, then
/// anchor.
pub(super) fn left(state: &ProjectState, unrepaired: &Unrepaired) -> Vec<Left> {
    if unrepaired.is_empty() {
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
