//! The decision ops: record, revise, rename, merge and remove, each
//! checked against the graph the earlier ops of its batch left.

use std::collections::BTreeSet;

use super::applied::Done;
use super::check::{self, Fault, Sources, anchor_edit, edit_list, new_name};
use super::context::Context;
use super::edit::Edit;
use super::ops::{Field, ListEdit, Merge, OpKind, Record, Remove, Rename, Revise};
use super::refusal::{NodeKind, Rule};
use crate::store::model::DecisionField;
use crate::store::model::{AcceptedText, Anchor, Decision, PROJECT_COMPONENT, Status};

pub(super) fn record(
    edit: &mut Edit<'_>,
    op: Record,
    batch: &mut Context<'_>,
) -> Result<Done, Fault> {
    new_name(edit, &op.name, Field::Name)?;
    component(edit, &op.component)?;
    let anchors = check::anchors(Field::Anchors, op.anchors)?;
    let mut depends_on = BTreeSet::new();
    let whole = ListEdit {
        whole: Some(op.depends_on),
        add: None,
        remove: None,
    };
    edit_list(&mut depends_on, whole, Field::DependsOn, |target| {
        dependency(edit, &op.name, target)
    })?;
    let gains_edges = !depends_on.is_empty();
    let sources = record_sources(&op.alternatives, &op.tags, &anchors);
    let decision = Decision {
        component: op.component,
        choice: op.choice,
        reason: op.reason,
        alternatives: op.alternatives,
        tags: op.tags.into_iter().collect(),
        status: Status::Proposed {
            accepted_text: None,
        },
        origin: batch.origin.map(str::to_owned),
        created: batch.created,
        anchors: anchors.into_iter().collect(),
        depends_on,
    };
    check::bar(batch, &decision, &DecisionField::IN_FILE_ORDER, &sources)?;
    // clone: the graph keys the node by its name, and the result names it.
    edit.put_decision(op.name.clone(), decision);
    // A dangling edge a hand edit left can point at the new name.
    if gains_edges {
        closes_no_cycle(edit, &op.name, Field::DependsOn)?;
    }
    Ok(Done::of(OpKind::Record, op.name))
}

/// A record's lists in the caller's order, for a verdict of the bar to
/// name.
fn record_sources(alternatives: &[String], tags: &[String], anchors: &[Anchor]) -> Sources {
    let mut sources = Sources::default();
    let rendered = anchors.iter().map(ToString::to_string).collect();
    sources.add(DecisionField::Anchors, Field::Anchors, rendered);
    // clone: the decision owns its lists, and the sources keep them beside it.
    let (alternatives, tags) = (alternatives.to_vec(), tags.to_vec());
    sources.add(
        DecisionField::Alternatives,
        Field::Alternatives,
        alternatives,
    );
    sources.add(DecisionField::Tags, Field::Tags, tags);
    sources
}

/// A revise is held to the bar in the fields whose values it changes: a
/// field it leaves as it is keeps what it held, below the bar or not.
pub(super) fn revise(
    edit: &mut Edit<'_>,
    op: Revise,
    batch: &mut Context<'_>,
) -> Result<Done, Fault> {
    // clone: the op is consumed by its revision, and the result names it.
    let name = op.name.clone();
    let mut sources = revise_sources(&op);
    let mut after = revised(edit, op, &mut sources)?;
    let before = existing(edit, &name, Field::Name)?;
    if after == *before {
        return Err(Fault::op(Rule::NoChange));
    }
    let changed = changed(before, &after);
    check::bar(batch, &after, &check::judged(&changed), &sources)?;
    restatus(before, &mut after);
    let gains_edges = !after.depends_on.is_subset(&before.depends_on);
    let status = (after.status.kind() != before.status.kind()).then(|| after.status.kind());
    // clone: the graph keys the node by its name, and the result names it.
    edit.put_decision(name.clone(), after);
    if gains_edges {
        closes_no_cycle(edit, &name, Field::DependsOn)?;
    }
    Ok(Done {
        changed,
        status,
        ..Done::of(OpKind::Revise, name)
    })
}

/// The text lists of `op` that bring elements into the decision; its
/// anchors join them once they are read as anchors (`revised`).
fn revise_sources(op: &Revise) -> Sources {
    let mut sources = Sources::default();
    // clone: the sources keep the caller's text beside the revision.
    let text = |text: &String| text.clone();
    sources.add_edit(
        DecisionField::Alternatives,
        Field::Alternatives,
        &op.alternatives,
        text,
    );
    sources.add_edit(DecisionField::Tags, Field::Tags, &op.tags, text);
    sources
}

/// The decision `op` names with each field `op` gives, its names and
/// lists checked; its status is the old one. Its anchors join `sources`,
/// as the bar reads them.
fn revised(edit: &Edit<'_>, op: Revise, sources: &mut Sources) -> Result<Decision, Fault> {
    // clone: the revision is built beside the value it is judged against.
    let mut after = existing(edit, &op.name, Field::Name)?.clone();
    if let Some(component) = op.component
        && component != after.component
    {
        self::component(edit, &component)?;
        after.component = component;
    }
    if let Some(choice) = op.choice {
        after.choice = choice;
    }
    if let Some(reason) = op.reason {
        after.reason = reason;
    }
    let free = |_: &_| None;
    edit_list(
        &mut after.alternatives,
        op.alternatives,
        Field::Alternatives,
        free,
    )?;
    edit_list(&mut after.tags, op.tags, Field::Tags, free)?;
    let anchors = anchor_edit(op.anchors)?;
    let (field, render) = (DecisionField::Anchors, Anchor::to_string);
    sources.add_edit(field, Field::Anchors, &anchors, render);
    edit_list(&mut after.anchors, anchors, Field::Anchors, |_| None)?;
    edit_list(
        &mut after.depends_on,
        op.depends_on,
        Field::DependsOn,
        |target| dependency(edit, &op.name, target),
    )?;
    Ok(after)
}

pub(super) fn rename(edit: &mut Edit<'_>, op: Rename) -> Result<Done, Fault> {
    existing(edit, &op.name, Field::Name)?;
    if op.to == op.name {
        return Err(Fault::op(Rule::NoChange));
    }
    new_name(edit, &op.to, Field::To)?;
    if let Some(mut decision) = edit.take_decision(&op.name) {
        repoint(&mut decision.depends_on, &op.name, Some(&op.to));
        // clone: the graph keys the node by its name, and the result names it.
        edit.put_decision(op.to.clone(), decision);
    }
    edit.carry(&op.name, &op.to);
    let rewrote = repoint_dependents(edit, &op.name, Some(&op.to));
    Ok(Done {
        to: Some(op.to),
        rewrote,
        ..Done::of(OpKind::Rename, op.name)
    })
}

/// A merge is held to the bar in the survivor's anchors and tags, when
/// the absorbed decision brings it ones it lacks.
pub(super) fn merge(
    edit: &mut Edit<'_>,
    op: Merge,
    batch: &mut Context<'_>,
) -> Result<Done, Fault> {
    existing(edit, &op.survivor, Field::Survivor)?;
    if op.absorbed == op.survivor {
        return Err(Fault::at(Field::Absorbed, Rule::SameNode));
    }
    existing(edit, &op.absorbed, Field::Absorbed)?;
    let absorbed = edit.take_decision(&op.absorbed);
    edit.carry(&op.absorbed, &op.survivor);
    if let (Some(absorbed), Some(survivor)) = (absorbed, edit.decision_mut(&op.survivor)) {
        let grown = [
            (
                DecisionField::Anchors,
                !absorbed.anchors.is_subset(&survivor.anchors),
            ),
            (
                DecisionField::Tags,
                !absorbed.tags.is_subset(&survivor.tags),
            ),
        ];
        survivor.anchors.extend(absorbed.anchors);
        survivor.tags.extend(absorbed.tags);
        survivor.depends_on.extend(absorbed.depends_on);
        survivor.depends_on.remove(&op.survivor);
        survivor.depends_on.remove(&op.absorbed);
        let held: Vec<DecisionField> = grown
            .into_iter()
            .filter_map(|(field, grew)| grew.then_some(field))
            .collect();
        // A merge brings no argument's elements: a fault names the field.
        check::bar(batch, survivor, &held, &Sources::default())?;
    }
    let rewrote = repoint_dependents(edit, &op.absorbed, Some(&op.survivor));
    closes_no_cycle(edit, &op.survivor, Field::Absorbed)?;
    Ok(Done {
        absorbed: Some(op.absorbed),
        rewrote,
        ..Done::of(OpKind::Merge, op.survivor)
    })
}

pub(super) fn remove(edit: &mut Edit<'_>, op: Remove) -> Result<Done, Fault> {
    existing(edit, &op.name, Field::Name)?;
    match &op.repoint_to {
        Some(heir) if *heir == op.name => {
            return Err(Fault::at(Field::RepointTo, Rule::SameNode));
        }
        Some(heir) => {
            existing(edit, heir, Field::RepointTo)?;
        }
        None => {
            let names = dependents(edit, &op.name);
            if !names.is_empty() {
                return Err(Fault::op(Rule::Dependents { names }));
            }
        }
    }
    edit.take_decision(&op.name);
    edit.end(&op.name);
    let rewrote = repoint_dependents(edit, &op.name, op.repoint_to.as_deref());
    if let Some(heir) = &op.repoint_to
        && rewrote > 0
    {
        closes_no_cycle(edit, heir, Field::RepointTo)?;
    }
    Ok(Done {
        repoint_to: op.repoint_to,
        rewrote,
        ..Done::of(OpKind::Remove, op.name)
    })
}

/// The decision `name`, given as `field`, as the graph holds it now.
fn existing<'e>(edit: &'e Edit<'_>, name: &str, field: Field) -> Result<&'e Decision, Fault> {
    edit.state().decisions.get(name).ok_or_else(|| {
        Fault::at(
            field,
            Rule::NotFound {
                name: name.to_owned(),
                kind: NodeKind::Decision,
            },
        )
    })
}

/// `component` names the project or a component.
fn component(edit: &Edit<'_>, component: &str) -> Result<(), Fault> {
    if component == PROJECT_COMPONENT || edit.state().components.contains_key(component) {
        return Ok(());
    }
    Err(Fault::at(
        Field::Component,
        Rule::ComponentMissing {
            component: component.to_owned(),
        },
    ))
}

/// Why `from` may not depend on `target`, if it may not.
fn dependency(edit: &Edit<'_>, from: &str, target: &str) -> Option<Rule> {
    if target == from {
        Some(Rule::SelfEdge)
    } else if edit.state().decisions.contains_key(target) {
        None
    } else {
        Some(Rule::DependencyMissing {
            target: target.to_owned(),
        })
    }
}

/// Refused when the edges the op added at `pivot` closed a cycle the batch
/// began without; `field` is the argument that added them.
fn closes_no_cycle(edit: &mut Edit<'_>, pivot: &str, field: Field) -> Result<(), Fault> {
    match edit.new_cycle(pivot) {
        Some(members) => Err(Fault::at(field, Rule::Cycle { members })),
        None => Ok(()),
    }
}

/// The decisions other than `name` that depend on it, by name.
fn dependents(edit: &Edit<'_>, name: &str) -> Vec<String> {
    edit.state()
        .decisions
        .iter()
        .filter(|(other, decision)| *other != name && decision.depends_on.contains(name))
        .map(|(other, _)| other.to_owned())
        .collect()
}

/// Point every edge at `from` at `to` instead, or drop it for `None`, and
/// count the decisions rewritten. An edge that would point at the decision
/// it leaves is dropped: the heir held what it depended on.
fn repoint_dependents(edit: &mut Edit<'_>, from: &str, to: Option<&str>) -> usize {
    let names = dependents(edit, from);
    for name in &names {
        let heir = to.filter(|heir| heir != name);
        if let Some(decision) = edit.decision_mut(name) {
            repoint(&mut decision.depends_on, from, heir);
        }
    }
    names.len()
}

fn repoint(depends_on: &mut BTreeSet<String>, from: &str, to: Option<&str>) {
    if depends_on.remove(from)
        && let Some(to) = to
    {
        depends_on.insert(to.to_owned());
    }
}

/// The status a revision leaves: a change of an accepted decision's text
/// keeps that text in `accepted_text` and proposes the new one; a later
/// change keeps the first `accepted_text`, and a change back to it accepts
/// the decision again as it was accepted. Anchors, tags, edges and the
/// component leave the status alone.
fn restatus(before: &Decision, after: &mut Decision) {
    let same_text = |choice: &str, reason: &str, alternatives: &[String]| {
        after.choice == choice && after.reason == reason && after.alternatives == alternatives
    };
    if same_text(&before.choice, &before.reason, &before.alternatives) {
        return;
    }
    let status = match &before.status {
        Status::Accepted { via } => Status::Proposed {
            accepted_text: Some(AcceptedText {
                // clone: the accepted text stays beside its revision.
                choice: before.choice.clone(),
                reason: before.reason.clone(),
                alternatives: before.alternatives.clone(),
                via: *via,
            }),
        },
        Status::Proposed {
            accepted_text: Some(text),
        } if same_text(&text.choice, &text.reason, &text.alternatives) => {
            Status::Accepted { via: text.via }
        }
        Status::Proposed { .. } => return,
    };
    after.status = status;
}

/// The fields whose values differ, in the order of the file's keys.
fn changed(before: &Decision, after: &Decision) -> Vec<Field> {
    [
        (Field::Component, before.component != after.component),
        (Field::Choice, before.choice != after.choice),
        (Field::Reason, before.reason != after.reason),
        (
            Field::Alternatives,
            before.alternatives != after.alternatives,
        ),
        (Field::Tags, before.tags != after.tags),
        (Field::Anchors, before.anchors != after.anchors),
        (Field::DependsOn, before.depends_on != after.depends_on),
    ]
    .into_iter()
    .filter_map(|(field, differs)| differs.then_some(field))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::store::apply::*;
    use crate::store::format::canonical;
    use crate::store::graph::NodeFile;
    use crate::store::model::{AcceptedText, AcceptedVia, Anchor, Status, StatusName};
    use crate::store::testing::{
        add, anchor, apply, decision, graph, record, refused, rename, revise, sample_decision,
        write_node,
    };
    use crate::store::{Field, Store};

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
}
