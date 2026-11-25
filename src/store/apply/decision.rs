//! The decision ops: record, revise, rename, merge and remove, each
//! checked against the graph the earlier ops of its batch left.

use std::collections::BTreeSet;

use super::Done;
use super::check::{self, Fault, Sources, anchor_edit, edit_list, new_name};
use super::edit::Edit;
use super::ops::{Field, ListEdit, Merge, OpKind, Record, Remove, Rename, Revise};
use super::patterns::Pending;
use super::refusal::{NodeKind, Rule};
use crate::store::quality::{Bar, DecisionField};
use crate::store::schema::{AcceptedText, Anchor, Decision, PROJECT_COMPONENT, Status};
use crate::store::time::Timestamp;

/// What the ops of one batch share: what every record carries, the bar
/// each written decision meets, and what the patterns judge once every op
/// has run.
pub(super) struct Context<'b> {
    pub(super) origin: Option<&'b str>,
    pub(super) created: Timestamp,
    pub(super) bar: &'b Bar,
    /// The index of the op running now.
    pub(super) op: usize,
    /// Each decision an op wrote, kept for the patterns, in op order.
    pub(super) pending: Vec<Pending>,
}

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
    check::bar(batch, &decision, &DecisionField::IN_FILE_ORDER, sources)?;
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
    check::bar(batch, &after, &check::judged(&changed), sources)?;
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
        check::bar(batch, survivor, &held, Sources::default())?;
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
