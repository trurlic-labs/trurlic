//! The `repair_anchors` op: stale anchors replaced in their decisions,
//! each refusal naming the repair and the part of it at fault, the
//! status of an accepted decision left alone, and one commit; a map of
//! repairs applied where the op runs, and the bar judged on what a repair
//! brings.

use std::collections::BTreeMap;
use std::fs;

use tempfile::TempDir;

use super::{anchor, apply, files, graph, named, refused};
use crate::store::apply::{Op, OpKind, Rename, Repair, RepairAnchors};
use crate::store::schema::{Anchor, Status};
use crate::store::state::NodeFile;
use crate::store::testing::{
    OUTGROWS_ITS_CACHE, changed_in, coin_flips, graph_log, sample_decision, shared_store,
    write_node,
};
use crate::store::{AnchorInput, Field, Store};

/// `sign` holds `src/old.rs::sign` and `src/kept.rs`, accepted; `rotate`
/// holds `src/old.rs::rotate`, proposed; `audit` holds `src/kept.rs`.
fn anchored(store: &Store) {
    let held = |anchors: &[(&str, Option<&str>)]| {
        anchors
            .iter()
            .map(|(file, symbol)| Anchor::new((*file).into(), symbol.map(str::to_owned)).unwrap())
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

/// A repair is judged on the anchors it brings: never on a count of the
/// whole list, which a replacement does not raise, so a decision already
/// past `anchors_max` takes one, nor on an anchor the decision held before,
/// which a repair to it brings none of. A pattern a listed repair breaks
/// refuses the batch, naming the decision at fault and the repair that
/// brought the anchor; one a repair of the map breaks names the decision
/// and its field, since no argument of the caller's holds the anchor.
#[test]
fn a_repair_is_judged_on_the_anchors_it_brings_and_names_its_decision() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    for (name, held_anchors) in [
        ("a", &["src/old.rs::kept"][..]),
        ("b", &["src/old.rs::a1", "src/lib.rs::base"][..]),
        ("c", &["src/old.rs::c1", "vendor/held.rs"][..]),
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
    let map = [
        ("src/old.rs::kept", "src/new.rs::kept"),
        ("src/old.rs::a1", "src/new.rs::a1"),
    ];

    apply(&store, vec![every(&map)]).unwrap();

    assert_eq!(anchors(&store, "b"), ["src/lib.rs::base", "src/new.rs::a1"]);
    let to_held = repair(
        "c",
        anchor("src/old.rs", Some("c1")),
        anchor("vendor/held.rs", None),
    );
    apply(&store, op(vec![to_held])).unwrap();
    assert_eq!(anchors(&store, "c"), ["vendor/held.rs"]);
    let listed = op(vec![
        repair(
            "a",
            anchor("src/new.rs", Some("kept")),
            anchor("src/x.rs", None),
        ),
        repair(
            "b",
            anchor("src/new.rs", Some("a1")),
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

    let mapped = [
        ("src/new.rs::kept", "src/x.rs"),
        ("src/new.rs::a1", "vendor/a1.rs"),
    ];
    let refusal = refused(&store, vec![every(&mapped)]);

    let (name, at, message) = named(&refusal);
    assert_eq!(
        (name, at.as_str()),
        ("pattern", "ops[0].anchors"),
        "{message}"
    );
    let target = refusal.op.as_ref().map(|op| op.target.as_str());
    assert_eq!(target, Some("b"), "{message}");
    assert_eq!(anchors(&store, "a"), ["src/new.rs::kept"]);
}

/// The anchors a repair brings are read over the batch's caches: a map
/// that repairs decisions to anchors of random letters, each of which fits
/// a pattern's cache alone but not together, is refused as
/// `pattern_gave_up` at the decision where the cache filled, and nothing
/// is written.
#[test]
fn a_repair_whose_pattern_cache_fills_names_the_decision() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let path = store.project_path();
    let text = fs::read_to_string(&path).unwrap();
    let table = format!(
        "[quality]\n[[quality.patterns]]\npattern = '{OUTGROWS_ITS_CACHE}'\n\
         fields = [\"anchors\"]\nlevel = \"warn\"\nmessage = \"Random.\""
    );
    fs::write(&path, format!("{text}\n{table}\n")).unwrap();
    let map: Vec<(String, String)> = (0..4)
        .map(|index| {
            let from = format!("src/old{index}.rs");
            let to = format!("src/{}.rs", coin_flips(480, index));
            let mut decision = sample_decision("project");
            decision.anchors = [held(&from)].into();
            write_node(&store, &NodeFile::Decision(format!("d{index}")), &decision);
            (from, to)
        })
        .collect();
    let map: Vec<(&str, &str)> = map.iter().map(|(f, t)| (f.as_str(), t.as_str())).collect();
    let before = files(&store);

    let refusal = refused(&store, vec![every(&map)]);

    let (name, at, message) = named(&refusal);
    assert_eq!(
        (name, at.as_str()),
        ("pattern_gave_up", "ops[0].anchors"),
        "{message}"
    );
    let target = refusal.op.as_ref().map(|op| op.target.as_str());
    assert!(target.is_some_and(|name| name > "d0"), "{message}");
    assert_eq!(files(&store), before);
}
