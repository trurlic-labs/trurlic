use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::store::canonical::canonical;
use crate::store::schema::{AcceptedText, AcceptedVia, Anchor, Decision, Status};
use crate::store::testing::{
    changed_in, graph_log, sample_component, sample_decision, setup_store, shared_store, write_node,
};
use crate::store::{AnchorFault, IssueKind, NameFault, Store, TextFault};

/// `auth` and `billing`; `sign` and `rotate` of `auth`, `rotate`
/// depending on `sign`; `audit`, project-wide, depending on `rotate`.
fn graph(dir: &Path) -> Store {
    let store = setup_store(dir);
    for name in ["auth", "billing"] {
        write_node(
            &store,
            &NodeFile::Component(name.into()),
            &sample_component(),
        );
    }
    let mut rotate = sample_decision("auth");
    rotate.depends_on.insert("sign".into());
    let mut audit = sample_decision("project");
    audit.depends_on.insert("rotate".into());
    for (name, decision) in [
        ("sign", sample_decision("auth")),
        ("rotate", rotate),
        ("audit", audit),
    ] {
        write_node(&store, &NodeFile::Decision(name.into()), &decision);
    }
    store
}

fn batch(ops: Vec<Op>) -> Batch {
    Batch {
        ops,
        origin: Some("row 7".into()),
        session: None,
    }
}

fn apply(store: &Store, ops: Vec<Op>) -> Result<Applied> {
    store.begin_write()?.apply(batch(ops))
}

/// The refusal of `ops`, after checking that it wrote nothing.
fn refused(store: &Store, ops: Vec<Op>) -> Refusal {
    let before = files(store);
    let err = apply(store, ops).unwrap_err();
    assert_eq!(files(store), before, "a refused batch wrote");
    match err {
        Error::Refused(refusal) => *refusal,
        other => panic!("not a refusal: {other}"),
    }
}

/// Every graph file below the store, by path.
fn files(store: &Store) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for dir in ["components", "decisions"] {
        for entry in fs::read_dir(store.root().join(dir)).unwrap() {
            let path = entry.unwrap().path();
            let key = format!("{dir}/{}", path.file_name().unwrap().to_str().unwrap());
            files.insert(key, fs::read_to_string(&path).unwrap());
        }
    }
    files
}

fn decision(store: &Store, name: &str) -> Decision {
    store.load_state().unwrap().decisions.remove(name).unwrap()
}

fn record(name: &str, component: &str) -> Record {
    Record {
        name: name.into(),
        component: component.into(),
        choice: format!("Choice of {name}"),
        reason: "The reason this choice holds".into(),
        alternatives: vec!["Another way: rejected".into()],
        tags: vec!["b".into(), "a".into()],
        anchors: vec![anchor("src/lib.rs", None)],
        depends_on: Vec::new(),
    }
}

fn anchor(file: &str, symbol: Option<&str>) -> AnchorInput {
    AnchorInput {
        file: file.into(),
        symbol: symbol.map(str::to_owned),
    }
}

fn revise(name: &str) -> Revise {
    Revise {
        name: name.into(),
        component: None,
        choice: None,
        reason: None,
        alternatives: ListEdit::keep(),
        tags: ListEdit::keep(),
        anchors: ListEdit::keep(),
        depends_on: ListEdit::keep(),
    }
}

fn add(elements: &[&str]) -> ListEdit<String> {
    ListEdit {
        add: Some(elements.iter().map(|&e| e.to_owned()).collect()),
        ..ListEdit::keep()
    }
}

fn rename(name: &str, to: &str) -> Rename {
    Rename {
        name: name.into(),
        to: to.into(),
    }
}

/// The rule name, the path and the message of `refusal`.
fn named(refusal: &Refusal) -> (&'static str, String, String) {
    (refusal.rule.as_str(), refusal.path(), refusal.to_string())
}

// -- record --------------------------------------------------------------

#[test]
fn a_record_writes_a_proposed_decision_with_the_batch_origin() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = record("expire", "auth");
    op.depends_on = vec!["sign".into()];

    let applied = apply(&store, vec![Op::Record(op)]).unwrap();

    assert_eq!(applied.ops, [Done::of(OpKind::Record, "expire".into())]);
    assert_eq!((applied.written, applied.removed), (1, 0));
    let written = decision(&store, "expire");
    assert_eq!(
        written.status,
        Status::Proposed {
            accepted_text: None
        }
    );
    assert_eq!(written.origin.as_deref(), Some("row 7"));
    assert_eq!(written.tags, ["a", "b"].map(String::from).into());
    assert_eq!(written.depends_on, ["sign"].map(String::from).into());
    let text = fs::read_to_string(store.decision_path("expire")).unwrap();
    assert_eq!(text, canonical(&written));
}

/// One instant per batch, and later ops see earlier ones: a component
/// added, then a decision in it depending on one recorded before it.
#[test]
fn a_batch_builds_on_its_own_earlier_ops() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut second = record("second", "ledger");
    second.depends_on = vec!["first".into()];
    let ops = vec![
        Op::AddComponent(AddComponent {
            name: "ledger".into(),
            description: "Books".into(),
            connects_to: vec!["auth".into()],
        }),
        Op::Record(record("first", "ledger")),
        Op::Record(second),
    ];

    let applied = apply(&store, ops).unwrap();

    assert_eq!(applied.written, 3);
    let state = store.load_state().unwrap();
    assert_eq!(state.validate(), []);
    assert_eq!(
        state.decisions["first"].created,
        state.decisions["second"].created
    );
}

#[test]
fn each_fault_of_a_record_is_refused_naming_its_argument() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let with = |change: fn(&mut Record)| {
        let mut op = record("expire", "auth");
        change(&mut op);
        vec![Op::Record(op)]
    };
    let cases: Vec<(Vec<Op>, &str, &str)> = vec![
        (
            with(|op| op.name = "Expire".into()),
            "name_invalid",
            "ops[0].name",
        ),
        (
            with(|op| op.name = "project".into()),
            "name_reserved",
            "ops[0].name",
        ),
        (
            with(|op| op.name = "sign".into()),
            "name_taken",
            "ops[0].name",
        ),
        (
            with(|op| op.name = "billing".into()),
            "name_taken",
            "ops[0].name",
        ),
        (
            with(|op| op.component = "ledger".into()),
            "component_missing",
            "ops[0].component",
        ),
        (
            with(|op| op.choice = "One\ntwo".into()),
            "invalid_text",
            "ops[0].choice",
        ),
        (
            with(|op| op.reason = "short".into()),
            "invalid_text",
            "ops[0].reason",
        ),
        (
            with(|op| op.anchors.push(anchor("../x.rs", None))),
            "invalid_anchor",
            "ops[0].anchors[1]",
        ),
        (
            with(|op| op.depends_on = vec!["sign".into(), "gone".into()]),
            "dependency_missing",
            "ops[0].depends_on[1]",
        ),
        (
            with(|op| op.depends_on = vec!["expire".into()]),
            "self_edge",
            "ops[0].depends_on[0]",
        ),
    ];
    for (ops, rule, path) in cases {
        let refusal = refused(&store, ops);
        assert_eq!(
            (refusal.rule.as_str(), refusal.path().as_str()),
            (rule, path)
        );
        let at = refusal.op.as_ref().unwrap();
        assert_eq!((at.index, at.kind), (0, OpKind::Record));
    }
}

#[test]
fn a_refusal_message_names_the_op_the_argument_and_the_rule() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = record("expire", "auth");
    op.depends_on = vec!["gone".into()];

    let refusal = refused(
        &store,
        vec![Op::Record(record("first", "auth")), Op::Record(op)],
    );

    assert_eq!(
        named(&refusal),
        (
            "dependency_missing",
            "ops[1].depends_on[0]".to_owned(),
            "ops[1] (record `expire`), `depends_on[0]`: names `gone`, which is no decision"
                .to_owned()
        )
    );
}

// -- revise --------------------------------------------------------------

#[test]
fn a_revise_sets_whole_lists_and_edits_others_by_element() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = revise("rotate");
    op.choice = Some("Rotate keys daily".into());
    op.tags = ListEdit {
        whole: Some(vec!["keys".into(), "keys".into()]),
        ..ListEdit::keep()
    };
    op.anchors = ListEdit {
        add: Some(vec![
            anchor("src/a.rs", Some("f")),
            anchor("src/a.rs", Some("f")),
        ]),
        ..ListEdit::keep()
    };
    op.depends_on = ListEdit {
        remove: Some(vec!["sign".into()]),
        add: Some(vec!["keys".into()]),
        ..ListEdit::keep()
    };

    let applied = apply(
        &store,
        vec![Op::Record(record("keys", "auth")), Op::Revise(op)],
    )
    .unwrap();

    let done = &applied.ops[1];
    assert_eq!(
        done.changed,
        [Field::Choice, Field::Tags, Field::Anchors, Field::DependsOn]
    );
    let rotate = decision(&store, "rotate");
    assert_eq!(rotate.tags, ["keys"].map(String::from).into());
    assert_eq!(rotate.anchors.len(), 1);
    assert_eq!(rotate.depends_on, ["keys"].map(String::from).into());
}

#[test]
fn a_revise_that_changes_nothing_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    // A hand edit out of canonical form, which only `fmt` rewrites.
    let text = fs::read_to_string(store.decision_path("sign")).unwrap();
    fs::write(store.decision_path("sign"), text.replace(" = ", "=")).unwrap();
    let mut same = revise("sign");
    same.choice = Some(sample_decision("auth").choice);
    same.tags = add(&[]);
    let mut back = revise("rotate");
    back.depends_on = ListEdit {
        remove: Some(vec!["sign".into()]),
        add: Some(vec!["sign".into()]),
        ..ListEdit::keep()
    };

    for op in [same, back] {
        let refusal = refused(&store, vec![Op::Revise(op)]);
        assert_eq!(refusal.rule, Rule::NoChange);
        assert_eq!(refusal.path(), "ops[0]");
    }
}

#[test]
fn a_list_given_whole_and_by_element_or_a_removal_of_what_is_absent_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut both = revise("sign");
    both.tags = ListEdit {
        whole: Some(vec!["a".into()]),
        remove: Some(vec!["b".into()]),
        add: None,
    };
    let mut absent = revise("sign");
    absent.anchors = ListEdit {
        remove: Some(vec![anchor("src/nowhere.rs", None)]),
        ..ListEdit::keep()
    };

    let refusal = refused(&store, vec![Op::Revise(both)]);
    assert_eq!(refusal.rule, Rule::BothForms { whole: Field::Tags });
    assert_eq!(refusal.path(), "ops[0].remove_tags");
    let refusal = refused(&store, vec![Op::Revise(absent)]);
    assert_eq!(named(&refusal).1, "ops[0].remove_anchors[0]", "{refusal}");
    assert_eq!(
        refusal.rule,
        Rule::NotHeld {
            element: "src/nowhere.rs".into()
        }
    );
}

#[test]
fn an_unnormalized_anchor_is_refused_in_any_list_form() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = revise("sign");
    op.anchors = ListEdit {
        add: Some(vec![anchor("src/a.rs", None), anchor("./src/b.rs", None)]),
        ..ListEdit::keep()
    };
    let refusal = refused(&store, vec![Op::Revise(op)]);
    assert_eq!(refusal.rule, Rule::Anchor(AnchorFault::Unnormalized));
    assert_eq!(refusal.path(), "ops[0].add_anchors[1]");
}

fn accepted_via(store: &Store, name: &str, via: Option<AcceptedVia>) {
    let mut decision = decision(store, name);
    decision.status = Status::Accepted { via };
    write_node(store, &NodeFile::Decision(name.into()), &decision);
}

/// A text change proposes the revision and keeps the accepted text; a
/// second keeps the first accepted text; a change back accepts it again,
/// as it was accepted. A change of anchors leaves the status alone.
#[test]
fn a_revision_of_an_accepted_text_is_proposed_until_it_returns() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    accepted_via(&store, "sign", Some(AcceptedVia::Cli));
    let accepted = decision(&store, "sign");
    let choose = |choice: &str| {
        let mut op = revise("sign");
        op.choice = Some(choice.into());
        vec![Op::Revise(op)]
    };
    let kept = Status::Proposed {
        accepted_text: Some(AcceptedText {
            choice: accepted.choice.clone(),
            reason: accepted.reason.clone(),
            alternatives: accepted.alternatives.clone(),
            via: Some(AcceptedVia::Cli),
        }),
    };

    let first = apply(&store, choose("Sign with Ed25519")).unwrap();
    assert_eq!(first.ops[0].status, Some(StatusName::Proposed));
    assert_eq!(decision(&store, "sign").status, kept);

    let second = apply(&store, choose("Sign with Ed448")).unwrap();
    assert_eq!(second.ops[0].status, None);
    assert_eq!(decision(&store, "sign").status, kept);

    let mut anchors = revise("sign");
    anchors.anchors = ListEdit {
        add: Some(vec![anchor("src/sign.rs", None)]),
        ..ListEdit::keep()
    };
    apply(&store, vec![Op::Revise(anchors)]).unwrap();
    assert_eq!(decision(&store, "sign").status, kept);

    let back = apply(&store, choose(&accepted.choice)).unwrap();
    assert_eq!(back.ops[0].status, Some(StatusName::Accepted));
    assert_eq!(
        decision(&store, "sign").status,
        Status::Accepted {
            via: Some(AcceptedVia::Cli)
        }
    );
}

#[test]
fn a_tag_or_edge_change_of_an_accepted_decision_keeps_it_accepted() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = revise("audit");
    op.tags = add(&["security"]);
    op.depends_on = add(&["sign"]);
    op.component = Some("billing".into());

    let applied = apply(&store, vec![Op::Revise(op)]).unwrap();

    assert_eq!(applied.ops[0].status, None);
    assert_eq!(
        decision(&store, "audit").status,
        Status::Accepted { via: None }
    );
}

// -- cycles --------------------------------------------------------------

#[test]
fn an_edge_that_closes_a_cycle_is_refused_naming_its_members() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut op = revise("sign");
    op.depends_on = add(&["audit"]);

    let refusal = refused(&store, vec![Op::Revise(op)]);

    assert_eq!(
        refusal.rule,
        Rule::Cycle {
            members: ["audit", "rotate", "sign"].map(String::from).into()
        }
    );
    assert_eq!(refusal.path(), "ops[0].depends_on");
    assert!(
        refusal
            .to_string()
            .ends_with("closes a depends_on cycle among `audit`, `rotate`, `sign`"),
        "{refusal}"
    );
}

/// A cycle a hand edit left is no new issue: an edge inside it does not
/// change its members, and a write elsewhere lands beside it.
#[test]
fn a_cycle_the_graph_began_with_blocks_no_write() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut sign = decision(&store, "sign");
    sign.depends_on.insert("audit".into());
    write_node(&store, &NodeFile::Decision("sign".into()), &sign);
    let mut inside = revise("sign");
    inside.depends_on = add(&["rotate"]);

    apply(
        &store,
        vec![Op::Revise(inside), Op::Record(record("new", "auth"))],
    )
    .unwrap();

    let state = store.load_state().unwrap();
    let kinds: Vec<IssueKind> = state.validate().iter().map(|issue| issue.kind).collect();
    assert_eq!(kinds, [IssueKind::DependsOnCycle]);
}

// -- rename, merge, remove -----------------------------------------------

#[test]
fn a_rename_moves_the_file_and_rewrites_every_edge_to_it() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());

    let applied = apply(&store, vec![Op::Rename(rename("rotate", "rotate-keys"))]).unwrap();

    assert_eq!(applied.ops[0].to.as_deref(), Some("rotate-keys"));
    assert_eq!(applied.ops[0].rewrote, 1);
    assert_eq!((applied.written, applied.removed), (2, 1));
    assert!(!store.decision_path("rotate").exists());
    let state = store.load_state().unwrap();
    assert_eq!(state.validate(), []);
    assert!(state.decisions["audit"].depends_on.contains("rotate-keys"));
    assert_eq!(
        state.decisions["rotate-keys"].status,
        Status::Accepted { via: None }
    );
}

#[test]
fn a_rename_to_its_own_name_or_a_taken_one_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let cases = [
        (rename("rotate", "rotate"), "no_change", "ops[0]"),
        (rename("rotate", "sign"), "name_taken", "ops[0].to"),
        (rename("rotate", "auth"), "name_taken", "ops[0].to"),
        (rename("gone", "new"), "not_found", "ops[0].name"),
    ];
    for (op, rule, path) in cases {
        let refusal = refused(&store, vec![Op::Rename(op)]);
        assert_eq!(
            (refusal.rule.as_str(), refusal.path().as_str()),
            (rule, path)
        );
    }
}

/// The survivor keeps its text and status and takes the absorbed
/// decision's anchors, tags and edges; edges to the absorbed one point at
/// the survivor.
#[test]
fn a_merge_repoints_edges_and_unions_anchors_and_tags() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut rotate = decision(&store, "rotate");
    rotate
        .anchors
        .insert(Anchor::new("src/rotate.rs".into(), None).unwrap());
    rotate.tags.insert("keys".into());
    rotate.status = Status::Proposed {
        accepted_text: None,
    };
    write_node(&store, &NodeFile::Decision("rotate".into()), &rotate);
    let sign = decision(&store, "sign");

    let applied = apply(
        &store,
        vec![Op::Merge(Merge {
            survivor: "sign".into(),
            absorbed: "rotate".into(),
        })],
    )
    .unwrap();

    assert_eq!(applied.ops[0].absorbed.as_deref(), Some("rotate"));
    assert_eq!(applied.ops[0].rewrote, 1);
    let state = store.load_state().unwrap();
    assert!(!state.decisions.contains_key("rotate"));
    let merged = &state.decisions["sign"];
    assert_eq!(
        (&merged.choice, &merged.status),
        (&sign.choice, &sign.status)
    );
    assert_eq!(merged.anchors, rotate.anchors);
    assert_eq!(merged.tags, rotate.tags);
    assert!(
        merged.depends_on.is_empty(),
        "the edge to itself is dropped"
    );
    assert_eq!(
        state.decisions["audit"].depends_on,
        ["sign"].map(String::from).into()
    );
    assert_eq!(state.validate(), []);
}

#[test]
fn a_merge_into_itself_or_one_that_closes_a_cycle_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let merge = |survivor: &str, absorbed: &str| {
        vec![Op::Merge(Merge {
            survivor: survivor.into(),
            absorbed: absorbed.into(),
        })]
    };
    let refusal = refused(&store, merge("sign", "sign"));
    assert_eq!(
        (refusal.path(), refusal.rule),
        ("ops[0].absorbed".into(), Rule::SameNode)
    );

    // `audit` takes `rotate`'s edge to `sign`, and `sign` would then
    // depend on `audit` through `x`.
    let mut x = record("x", "auth");
    x.depends_on = vec!["audit".into()];
    let mut sign = revise("sign");
    sign.depends_on = add(&["x"]);
    let mut ops = vec![Op::Record(x)];
    ops.extend(merge("audit", "rotate"));
    ops.push(Op::Revise(sign));
    let refusal = refused(&store, ops);
    assert_eq!(refusal.op.as_ref().unwrap().index, 2);
    assert_eq!(
        refusal.rule,
        Rule::Cycle {
            members: ["audit", "sign", "x"].map(String::from).into()
        }
    );
}

#[test]
fn a_remove_with_dependents_is_refused_unless_it_repoints_them() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let remove = |name: &str, repoint_to: Option<&str>| {
        vec![Op::Remove(Remove {
            name: name.into(),
            repoint_to: repoint_to.map(str::to_owned),
        })]
    };

    let refusal = refused(&store, remove("sign", None));
    assert_eq!(
        refusal.rule,
        Rule::Dependents {
            names: vec!["rotate".into()]
        }
    );
    let refusal = refused(&store, remove("sign", Some("sign")));
    assert_eq!(refusal.path(), "ops[0].repoint_to");

    let applied = apply(&store, remove("rotate", Some("sign"))).unwrap();
    assert_eq!(applied.ops[0].rewrote, 1);
    assert_eq!((applied.written, applied.removed), (1, 1));
    let state = store.load_state().unwrap();
    assert_eq!(
        state.decisions["audit"].depends_on,
        ["sign"].map(String::from).into()
    );
    assert_eq!(state.validate(), []);
}

#[test]
fn a_repoint_that_closes_a_cycle_is_refused_and_an_heir_drops_its_own_edge() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut sign = revise("sign");
    sign.depends_on = add(&["billing-rule"]);
    let ops = vec![
        Op::Record(record("billing-rule", "billing")),
        Op::Revise(sign),
        Op::Remove(Remove {
            name: "billing-rule".into(),
            repoint_to: Some("audit".into()),
        }),
    ];
    let refusal = refused(&store, ops);
    assert_eq!(
        (refusal.op.as_ref().unwrap().index, refusal.path()),
        (2, "ops[2].repoint_to".into())
    );
    assert_eq!(refusal.rule.as_str(), "depends_on_cycle");

    apply(
        &store,
        vec![Op::Remove(Remove {
            name: "sign".into(),
            repoint_to: Some("rotate".into()),
        })],
    )
    .unwrap();
    assert!(decision(&store, "rotate").depends_on.is_empty());
}

// -- components ----------------------------------------------------------

#[test]
fn a_component_rename_carries_its_decisions_and_connections() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut billing = sample_component();
    billing.connects_to.insert("auth".into());
    write_node(&store, &NodeFile::Component("billing".into()), &billing);

    let applied = apply(
        &store,
        vec![Op::RenameComponent(rename("auth", "identity"))],
    )
    .unwrap();

    assert_eq!(applied.ops[0].rewrote, 3);
    let state = store.load_state().unwrap();
    assert_eq!(state.validate(), []);
    assert_eq!(state.decisions_of("identity").len(), 2);
    assert!(state.components["billing"].connects_to.contains("identity"));
    assert!(!store.component_path("auth").exists());
}

#[test]
fn a_component_in_use_is_not_removed() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let remove = |name: &str| vec![Op::RemoveComponent(RemoveComponent { name: name.into() })];

    let refusal = refused(&store, remove("auth"));
    assert_eq!(
        refusal.rule,
        Rule::InUse {
            decisions: vec!["rotate".into(), "sign".into()],
            components: vec![]
        }
    );
    apply(&store, remove("billing")).unwrap();
    assert!(!store.component_path("billing").exists());
}

#[test]
fn a_component_revise_checks_its_connections_and_refuses_no_change() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let revise = |connects_to: ListEdit<String>, description: Option<&str>| {
        vec![Op::ReviseComponent(ReviseComponent {
            name: "auth".into(),
            description: description.map(str::to_owned),
            connects_to,
        })]
    };
    let cases = [
        (
            revise(add(&["ledger"]), None),
            "connection_missing",
            "ops[0].add_connects_to[0]",
        ),
        (
            revise(add(&["auth"]), None),
            "self_edge",
            "ops[0].add_connects_to[0]",
        ),
        (
            revise(ListEdit::keep(), Some(&sample_component().description)),
            "no_change",
            "ops[0]",
        ),
    ];
    for (ops, rule, path) in cases {
        let refusal = refused(&store, ops);
        assert_eq!(
            (refusal.rule.as_str(), refusal.path().as_str()),
            (rule, path)
        );
    }
    let applied = apply(
        &store,
        revise(add(&["billing", "project"]), Some("Sign-in")),
    )
    .unwrap();
    assert_eq!(
        applied.ops[0].changed,
        [Field::Description, Field::ConnectsTo]
    );
}

// -- the batch -----------------------------------------------------------

/// The writer's graph is the loaded one after a refusal, and the files
/// on disk after a commit.
#[test]
fn a_refused_batch_leaves_the_writers_graph_as_it_was() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut writer = store.begin_write().unwrap();
    let loaded = writer.state().clone();
    let ops = vec![
        Op::Rename(rename("sign", "sign-tokens")),
        Op::RemoveComponent(RemoveComponent {
            name: "auth".into(),
        }),
    ];

    writer.apply(batch(ops)).unwrap_err();

    assert_eq!(writer.state().decisions, loaded.decisions);
    assert_eq!(writer.state().components, loaded.components);
    writer
        .apply(batch(vec![Op::Rename(rename("sign", "sign-tokens"))]))
        .unwrap();
    let committed = writer.state().clone();
    drop(writer);
    let on_disk = store.load_state().unwrap();
    assert_eq!(committed.decisions, on_disk.decisions);
    assert_eq!(committed.non_canonical, on_disk.non_canonical);
}

/// Only the files whose value changes are written: a node renamed away
/// and back, and a hand edit no op touches, keep their bytes.
#[test]
fn only_the_files_whose_value_changes_are_written() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let hand_edit = fs::read_to_string(store.component_path("billing"))
        .unwrap()
        .replace(" = ", "=");
    fs::write(store.component_path("billing"), &hand_edit).unwrap();
    let ops = vec![
        Op::Rename(rename("sign", "sign-tokens")),
        Op::Rename(rename("sign-tokens", "sign")),
        Op::Record(record("new", "auth")),
    ];

    let applied = apply(&store, ops).unwrap();

    assert_eq!((applied.written, applied.removed), (1, 0));
    assert_eq!(
        fs::read_to_string(store.component_path("billing")).unwrap(),
        hand_edit
    );
    let state = store.load_state().unwrap();
    assert_eq!(state.non_canonical, [NodeFile::Component("billing".into())]);
}

/// The final check refuses what no op rule did: a renamed decision
/// carries an edge a hand edit left dangling, which is an issue of a new
/// subject.
#[test]
fn the_resulting_graph_adds_no_issue_and_a_refusal_names_the_op_behind_it() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut sign = decision(&store, "sign");
    sign.depends_on.insert("gone".into());
    write_node(&store, &NodeFile::Decision("sign".into()), &sign);
    let ops = vec![
        Op::Rename(rename("sign", "sign-tokens")),
        Op::Record(record("new", "auth")),
    ];

    let refusal = refused(&store, ops);

    assert_eq!(refusal.op.as_ref().unwrap().index, 0);
    assert_eq!(refusal.rule.as_str(), "dependency_missing");
    assert!(
        refusal
            .to_string()
            .contains("the graph would gain an issue: decisions/sign-tokens.toml"),
        "{refusal}"
    );
    let mut unrelated = revise("rotate");
    unrelated.tags = add(&["keys"]);
    apply(&store, vec![Op::Revise(unrelated)]).unwrap();
}

#[test]
fn an_empty_batch_and_an_origin_of_two_lines_are_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());

    let refusal = refused(&store, vec![]);
    assert_eq!(
        (refusal.path(), refusal.rule),
        ("ops".into(), Rule::EmptyBatch)
    );

    let mut forged = batch(vec![Op::Record(record("new", "auth"))]);
    forged.origin = Some("row 7\nSession: forged".into());
    let err = store.begin_write().unwrap().apply(forged).unwrap_err();
    let Error::Refused(refusal) = err else {
        panic!("{err}");
    };
    assert_eq!(refusal.path(), "origin");
    assert_eq!(
        refusal.rule,
        Rule::Text {
            field: crate::store::TextField::Origin,
            fault: TextFault::LineBreak { at: 5 }
        }
    );
}

#[test]
fn a_name_longer_than_a_file_name_allows_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let long = "a".repeat(129);
    let refusal = refused(&store, vec![Op::Record(record(&long, "auth"))]);
    assert_eq!(refusal.rule, Rule::Name(NameFault::TooLong { len: 129 }));
}

/// Under `shared` a batch is one history commit, named `apply`, with the
/// batch's origin, holding exactly the files it changed.
#[test]
fn a_batch_is_one_history_commit_of_its_files() {
    let tmp = TempDir::new().unwrap();
    let store = shared_store(tmp.path());
    let ops = vec![
        Op::AddComponent(AddComponent {
            name: "auth".into(),
            description: "Auth".into(),
            connects_to: vec![],
        }),
        Op::Record(record("sign", "auth")),
        Op::Record(record("rotate", "auth")),
    ];
    apply(&store, ops).unwrap();
    apply(&store, vec![Op::Rename(rename("rotate", "rotate-keys"))]).unwrap();

    assert_eq!(
        graph_log(&store),
        ["apply\n\nOrigin: row 7", "apply\n\nOrigin: row 7", "init"]
    );
    assert_eq!(
        changed_in(&store, "HEAD"),
        ["decisions/rotate-keys.toml", "decisions/rotate.toml"]
    );
    assert_eq!(changed_in(&store, "HEAD~1").len(), 3);
}

mod quality;
