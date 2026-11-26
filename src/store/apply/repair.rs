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
use crate::store::model::DecisionField;
use crate::store::model::ProjectState;
use crate::store::model::{Anchor, Decision};
use crate::store::quality::{Bar, Breach, Hit, Place, Scan, texts};

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

/// Each refusal of a repair naming the repair and the part of it at fault,
/// the status of an accepted decision left alone, one commit, a map of
/// repairs applied where the op runs, and the bar judged on what a repair
/// brings.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::store::apply::*;
    use crate::store::model::{Anchor, NodeFile, Status};
    use crate::store::testing::{
        OUTGROWS_ITS_CACHE, anchor, apply, changed_in, coin_flips, files, graph, graph_log, named,
        refused, sample_decision, shared_store, write_node,
    };
    use crate::store::{AnchorInput, Field, Store};

    /// `sign` holds `src/old.rs::sign` and `src/kept.rs`, accepted; `rotate`
    /// holds `src/old.rs::rotate`, proposed; `audit` holds `src/kept.rs`.
    fn anchored(store: &Store) {
        let held = |anchors: &[(&str, Option<&str>)]| {
            anchors
                .iter()
                .map(|(file, symbol)| {
                    Anchor::new((*file).into(), symbol.map(str::to_owned)).unwrap()
                })
                .collect()
        };
        let mut sign = sample_decision("project");
        sign.anchors = held(&[("src/old.rs", Some("sign")), ("src/kept.rs", None)]);
        let mut rotate = sample_decision("project");
        rotate.anchors = held(&[("src/old.rs", Some("rotate"))]);
        rotate.status = Status::Proposed {
            accepted_text: None,
        };
        let mut audit = sample_decision("project");
        audit.anchors = held(&[("src/kept.rs", None)]);
        for (name, decision) in [("sign", sign), ("rotate", rotate), ("audit", audit)] {
            write_node(store, &NodeFile::Decision(name.into()), &decision);
        }
    }

    fn repair(decision: &str, from: AnchorInput, to: AnchorInput) -> Repair {
        Repair {
            decision: decision.into(),
            from,
            to,
        }
    }

    fn op(repairs: Vec<Repair>) -> Vec<Op> {
        vec![Op::RepairAnchors(RepairAnchors::Listed(repairs))]
    }

    /// `spelled`, `file` or `file::symbol`, as an anchor.
    fn held(spelled: &str) -> Anchor {
        let (file, symbol) = match spelled.split_once("::") {
            Some((file, symbol)) => (file, Some(symbol.to_owned())),
            None => (spelled, None),
        };
        Anchor::new(file.to_owned(), symbol).unwrap()
    }

    /// An op with every repair of `map`, each stale anchor spelled with
    /// its repair.
    fn every(map: &[(&str, &str)]) -> Op {
        let map: BTreeMap<Anchor, Anchor> = map
            .iter()
            .map(|(from, to)| (held(from), held(to)))
            .collect();
        Op::RepairAnchors(RepairAnchors::Every(map))
    }

    fn anchors(store: &Store, name: &str) -> Vec<String> {
        let decision = store.load_state().unwrap().decisions.remove(name).unwrap();
        decision.anchors.iter().map(ToString::to_string).collect()
    }

    /// Two decisions repaired by one op: their anchors replaced, an accepted
    /// decision still accepted, only their files written, in one commit; a
    /// repair to an anchor the decision holds already drops the stale one.
    #[test]
    fn a_repair_replaces_anchors_and_leaves_the_status_alone() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        anchored(&store);
        let before = files(&store);

        let applied = apply(
            &store,
            op(vec![
                repair(
                    "sign",
                    anchor("src/old.rs", Some("sign")),
                    anchor("src/new.rs", Some("sign")),
                ),
                repair(
                    "rotate",
                    anchor("src/old.rs", Some("rotate")),
                    anchor("src/new.rs", Some("Keys::rotate")),
                ),
                repair(
                    "sign",
                    anchor("src/kept.rs", None),
                    anchor("src/new.rs", Some("sign")),
                ),
            ]),
        )
        .unwrap();

        let done = &applied.ops[0];
        assert_eq!(
            (done.op, done.name.as_str()),
            (OpKind::RepairAnchors, "sign")
        );
        assert_eq!(
            (done.changed.as_slice(), done.rewrote),
            (&[Field::Anchors][..], 1)
        );
        assert_eq!(done.status, None);
        let written: Vec<(&str, String, String)> = applied
            .repaired
            .iter()
            .map(|r| (r.decision.as_str(), r.from.to_string(), r.to.to_string()))
            .collect();
        assert_eq!(
            written,
            [
                ("sign", "src/old.rs::sign".into(), "src/new.rs::sign".into()),
                (
                    "rotate",
                    "src/old.rs::rotate".into(),
                    "src/new.rs::Keys::rotate".into()
                ),
                ("sign", "src/kept.rs".into(), "src/new.rs::sign".into()),
            ]
        );
        assert_eq!(anchors(&store, "sign"), ["src/new.rs::sign"]);
        assert_eq!(anchors(&store, "rotate"), ["src/new.rs::Keys::rotate"]);
        let state = store.load_state().unwrap();
        assert!(matches!(
            state.decisions["sign"].status,
            Status::Accepted { .. }
        ));
        let after = files(&store);
        let changed: Vec<&String> = after
            .keys()
            .filter(|key| after[*key] != before[*key])
            .collect();
        assert_eq!(changed, ["decisions/rotate.toml", "decisions/sign.toml"]);
        assert_eq!(graph_log(&store)[0], "apply\n\nOrigin: row 7");
        assert_eq!(
            changed_in(&store, "HEAD"),
            ["decisions/rotate.toml", "decisions/sign.toml"]
        );
    }

    /// Each fault names the repair by its index and the part of it at fault;
    /// a fault in a later repair undoes the earlier ones: nothing is written.
    #[test]
    fn each_fault_of_a_repair_is_refused_naming_it() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        anchored(&store);
        let held = || anchor("src/old.rs", Some("sign"));
        let new = || anchor("src/new.rs", Some("sign"));
        let good = || repair("sign", held(), new());
        let cases: [(Vec<Repair>, &str, &str); 6] = [
            (vec![], "no_repair", "ops[0].repairs"),
            (
                vec![good(), repair("gone", held(), new())],
                "not_found",
                "ops[0].repairs[1].decision",
            ),
            (vec![good(), good()], "not_held", "ops[0].repairs[1].from"),
            (
                vec![repair("sign", anchor("src/../x.rs", None), new())],
                "invalid_anchor",
                "ops[0].repairs[0].from",
            ),
            (
                vec![repair("sign", held(), anchor("/abs.rs", None))],
                "invalid_anchor",
                "ops[0].repairs[0].to",
            ),
            (
                vec![repair("sign", held(), held())],
                "no_change",
                "ops[0].repairs[0].to",
            ),
        ];
        for (repairs, rule, path) in cases {
            let refusal = refused(&store, op(repairs));
            let (name, at, _) = named(&refusal);
            assert_eq!((name, at.as_str()), (rule, path));
        }
    }

    /// A map of repairs is applied where its op runs, to every decision ever
    /// accepted that holds a stale anchor then: after a rename it repairs the
    /// decision under its new name, a second such op finds nothing left and
    /// repairs nothing, refusing nothing, and a decision never accepted keeps
    /// its planned anchor. A map that matches nothing writes nothing.
    #[test]
    fn a_map_of_repairs_applies_where_its_op_runs() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        anchored(&store);
        let map = [
            ("src/old.rs::sign", "src/new.rs::sign"),
            ("src/old.rs::rotate", "src/new.rs::rotate"),
        ];
        let rename = Op::Rename(Rename {
            name: "sign".into(),
            to: "signed".into(),
        });

        let applied = apply(&store, vec![rename, every(&map), every(&map)]).unwrap();

        let named: Vec<(&str, &[Field])> = applied.ops[1..]
            .iter()
            .map(|done| (done.name.as_str(), done.changed.as_slice()))
            .collect();
        assert_eq!(named, [("signed", &[Field::Anchors][..]), ("", &[][..])]);
        assert_eq!(
            anchors(&store, "signed"),
            ["src/kept.rs", "src/new.rs::sign"]
        );
        assert_eq!(anchors(&store, "rotate"), ["src/old.rs::rotate"]);
        assert_eq!(applied.repaired.len(), 1);

        let before = files(&store);
        let idle = apply(&store, vec![every(&map)]).unwrap();
        assert_eq!(
            (idle.written, idle.removed, idle.ops[0].name.as_str()),
            (0, 0, "")
        );
        assert_eq!(files(&store), before);
    }

    /// The decisions `a` to `e`, each holding the anchors listed, under
    /// `anchors_max = 1` and a `refuse` pattern for `vendor/` over anchors.
    fn vendored(tmp: &TempDir) -> Store {
        let store = graph(tmp.path());
        for (name, held_anchors) in [
            ("a", &["src/old.rs::kept"][..]),
            ("b", &["src/old.rs::a1", "src/lib.rs::base"][..]),
            ("c", &["src/old.rs::c1", "vendor/held.rs"][..]),
            ("d", &["src/old.rs::d1", "vendor/held.rs"][..]),
            ("e", &["src/old.rs::e1", "vendor/held.rs"][..]),
        ] {
            let mut decision = sample_decision("project");
            decision.anchors = held_anchors.iter().map(|spelled| held(spelled)).collect();
            write_node(&store, &NodeFile::Decision(name.into()), &decision);
        }
        let path = store.project_path();
        let text = fs::read_to_string(&path).unwrap();
        let table = "[quality]\nanchors_max = 1\n[[quality.patterns]]\npattern = 'vendor/'\n\
                     fields = [\"anchors\"]\nlevel = \"refuse\"\nmessage = \"No vendored code.\"";
        fs::write(&path, format!("{text}\n{table}\n")).unwrap();
        store
    }

    /// A repair is judged on the anchor it brings alone: never on a count of
    /// the whole list, which a replacement does not raise, so a decision past
    /// `anchors_max` takes one; nor on an anchor the decision held before,
    /// whether the repair was listed or came of the map, so one holding an
    /// anchor a pattern refuses takes one; nor on an anchor it held already,
    /// which a repair to it brings none of.
    #[test]
    fn a_repair_is_judged_on_the_anchor_it_brings_alone() {
        let tmp = TempDir::new().unwrap();
        let store = vendored(&tmp);
        let map = [
            ("src/old.rs::kept", "src/new.rs::kept"),
            ("src/old.rs::a1", "src/new.rs::a1"),
            ("src/old.rs::d1", "src/new.rs::d1"),
        ];

        apply(&store, vec![every(&map)]).unwrap();
        let to_clean = repair(
            "c",
            anchor("src/old.rs", Some("c1")),
            anchor("src/new.rs", Some("c1")),
        );
        let to_held = repair(
            "e",
            anchor("src/old.rs", Some("e1")),
            anchor("vendor/held.rs", None),
        );
        apply(&store, op(vec![to_clean, to_held])).unwrap();

        assert_eq!(anchors(&store, "b"), ["src/lib.rs::base", "src/new.rs::a1"]);
        assert_eq!(anchors(&store, "c"), ["src/new.rs::c1", "vendor/held.rs"]);
        assert_eq!(anchors(&store, "d"), ["src/new.rs::d1", "vendor/held.rs"]);
        assert_eq!(anchors(&store, "e"), ["vendor/held.rs"]);
    }

    /// A pattern a listed repair breaks refuses the batch, naming the decision
    /// at fault and the repair that brought the anchor: the caller wrote it.
    #[test]
    fn a_listed_repair_a_pattern_refuses_refuses_the_batch() {
        let tmp = TempDir::new().unwrap();
        let store = vendored(&tmp);
        let listed = op(vec![
            repair(
                "a",
                anchor("src/old.rs", Some("kept")),
                anchor("src/x.rs", None),
            ),
            repair(
                "b",
                anchor("src/old.rs", Some("a1")),
                anchor("vendor/a1.rs", None),
            ),
        ]);

        let refusal = refused(&store, listed);

        let (name, at, message) = named(&refusal);
        assert_eq!(
            (name, at.as_str()),
            ("pattern", "ops[0].repairs[1]"),
            "{message}"
        );
        let target = refusal.op.as_ref().map(|op| op.target.as_str());
        assert_eq!(target, Some("b"), "{message}");
        assert!(
            message.starts_with("ops[0] (repair_anchors `b`)"),
            "{message}"
        );
    }

    /// A repair of the map a pattern refuses is left, its rule given, and the
    /// others are written: the caller wrote none of them.
    #[test]
    fn a_repair_of_the_map_a_pattern_refuses_is_left_with_its_rule() {
        let tmp = TempDir::new().unwrap();
        let store = vendored(&tmp);
        let mapped = [
            ("src/old.rs::kept", "src/x.rs"),
            ("src/old.rs::a1", "vendor/a1.rs"),
        ];

        let applied = apply(&store, vec![every(&mapped)]).unwrap();

        assert_eq!(anchors(&store, "a"), ["src/x.rs"]);
        assert_eq!(anchors(&store, "b"), ["src/lib.rs::base", "src/old.rs::a1"]);
        let left: Vec<(&str, &str)> = applied
            .left
            .iter()
            .map(|left| (left.repaired.decision.as_str(), left.rule.as_str()))
            .collect();
        assert_eq!(left, [("b", "pattern")]);
    }

    /// Two stale anchors of one decision whose repairs in the map are one
    /// anchor, which a pattern refuses: the decision is judged once on what it
    /// gained, and both repairs are left, the decision holding both stale
    /// anchors and the result naming no repair.
    #[test]
    fn two_repairs_of_the_map_to_one_refused_anchor_are_left_together() {
        let tmp = TempDir::new().unwrap();
        let store = vendored(&tmp);
        let mut decision = sample_decision("project");
        decision.anchors = [held("src/p.rs::x"), held("src/q.rs::x")].into();
        write_node(&store, &NodeFile::Decision("z".into()), &decision);
        let map = [
            ("src/p.rs::x", "vendor/x.rs"),
            ("src/q.rs::x", "vendor/x.rs"),
        ];

        let applied = apply(&store, vec![every(&map)]).unwrap();

        assert_eq!(anchors(&store, "z"), ["src/p.rs::x", "src/q.rs::x"]);
        assert_eq!(applied.repaired, []);
        let left: Vec<(String, &str)> = applied
            .left
            .iter()
            .map(|left| (left.repaired.from.to_string(), left.rule.as_str()))
            .collect();
        assert_eq!(
            left,
            [
                ("src/p.rs::x".to_owned(), "pattern"),
                ("src/q.rs::x".to_owned(), "pattern")
            ]
        );
    }

    /// The decision `dec-a` holding `src/old.rs`, under a pattern no cache
    /// holds over random letters, at `warn` over anchors and reason.
    fn outgrown(tmp: &TempDir) -> Store {
        let store = graph(tmp.path());
        let path = store.project_path();
        let text = fs::read_to_string(&path).unwrap();
        let table = format!(
            "[quality]\n[[quality.patterns]]\npattern = '{OUTGROWS_ITS_CACHE}'\n\
             fields = [\"anchors\", \"reason\"]\nlevel = \"warn\"\nmessage = \"Random.\""
        );
        fs::write(&path, format!("{text}\n{table}\n")).unwrap();
        let mut decision = sample_decision("project");
        decision.anchors = [held("src/old.rs")].into();
        write_node(&store, &NodeFile::Decision("dec-a".into()), &decision);
        store
    }

    /// A record whose reason is `letters` random letters drawn by `seed`.
    fn random(name: &str, letters: usize, seed: u64) -> Op {
        let mut record = crate::store::testing::record(name, "auth");
        record.reason = coin_flips(letters, seed);
        Op::Record(record)
    }

    /// The anchors a repair of the map brings are read over the batch's
    /// caches, after every earlier op: one whose pattern gives up, here after
    /// a record of random letters filled its cache, is left with the give-up
    /// as its rule. It leaves no trace: the record after it, which the batch
    /// writes without the repair, is written with it too.
    #[test]
    fn a_repair_of_the_map_a_pattern_gives_up_on_is_left_and_a_later_op_written() {
        let to = format!("src/{}/{}.rs", coin_flips(240, 1), coin_flips(240, 2));
        let records = || [random("warmer", 3_200, 3), random("second", 300, 9)];
        let tmp = TempDir::new().unwrap();
        let [warmer, second] = records();
        apply(&outgrown(&tmp), vec![warmer, second]).unwrap();
        let tmp = TempDir::new().unwrap();
        let store = outgrown(&tmp);
        let [warmer, second] = records();

        let applied = apply(&store, vec![warmer, every(&[("src/old.rs", &to)]), second]).unwrap();

        assert_eq!(applied.written, 2);
        assert_eq!(anchors(&store, "dec-a"), ["src/old.rs"]);
        let left: Vec<(&str, &str)> = applied
            .left
            .iter()
            .map(|left| (left.repaired.decision.as_str(), left.rule.as_str()))
            .collect();
        assert_eq!(left, [("dec-a", "pattern_gave_up")]);
    }
}
