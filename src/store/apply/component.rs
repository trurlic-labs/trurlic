//! The component ops: add, revise, rename and remove. A rename carries
//! its decisions and the connections to it; a remove is refused while
//! either remains.

use std::collections::BTreeSet;

use super::Done;
use super::check::{Fault, edit_list, new_name};
use super::edit::Edit;
use super::ops::{AddComponent, Field, ListEdit, OpKind, RemoveComponent, Rename, ReviseComponent};
use super::refusal::{NodeKind, Rule};
use crate::store::schema::{Component, PROJECT_COMPONENT};
use crate::store::text::{TextField, fault};

pub(super) fn add(edit: &mut Edit<'_>, op: AddComponent) -> Result<Done, Fault> {
    new_name(edit, &op.name, Field::Name)?;
    description(&op.description)?;
    let mut connects_to = BTreeSet::new();
    let whole = ListEdit {
        whole: Some(op.connects_to),
        add: None,
        remove: None,
    };
    edit_list(&mut connects_to, whole, Field::ConnectsTo, |target| {
        connection(edit, &op.name, target)
    })?;
    let component = Component {
        description: op.description,
        connects_to,
    };
    // clone: the graph keys the node by its name, and the result names it.
    edit.put_component(op.name.clone(), component);
    Ok(Done::of(OpKind::AddComponent, op.name))
}

pub(super) fn revise(edit: &mut Edit<'_>, op: ReviseComponent) -> Result<Done, Fault> {
    // clone: the revision is built beside the value it is judged against.
    let mut after = existing(edit, &op.name, Field::Name)?.clone();
    if let Some(text) = op.description {
        description(&text)?;
        after.description = text;
    }
    edit_list(
        &mut after.connects_to,
        op.connects_to,
        Field::ConnectsTo,
        |target| connection(edit, &op.name, target),
    )?;
    let before = existing(edit, &op.name, Field::Name)?;
    if after == *before {
        return Err(Fault::op(Rule::NoChange));
    }
    let changed = [
        (Field::Description, before.description != after.description),
        (Field::ConnectsTo, before.connects_to != after.connects_to),
    ]
    .into_iter()
    .filter_map(|(field, differs)| differs.then_some(field))
    .collect();
    // clone: the graph keys the node by its name, and the result names it.
    edit.put_component(op.name.clone(), after);
    Ok(Done {
        changed,
        ..Done::of(OpKind::ReviseComponent, op.name)
    })
}

pub(super) fn rename(edit: &mut Edit<'_>, op: Rename) -> Result<Done, Fault> {
    existing(edit, &op.name, Field::Name)?;
    if op.to == op.name {
        return Err(Fault::op(Rule::NoChange));
    }
    new_name(edit, &op.to, Field::To)?;
    if let Some(mut component) = edit.take_component(&op.name) {
        if component.connects_to.remove(&op.name) {
            // clone: the self-edge and the graph's key each own the name.
            component.connects_to.insert(op.to.clone());
        }
        // clone: the graph keys the node by its name, and the result names it.
        edit.put_component(op.to.clone(), component);
    }
    edit.carry(&op.name, &op.to);
    let (decisions, components) = uses(edit, &op.name);
    for name in &decisions {
        if let Some(decision) = edit.decision_mut(name) {
            // clone: each decision of the component owns its name.
            decision.component.clone_from(&op.to);
        }
    }
    for name in &components {
        if let Some(component) = edit.component_mut(name) {
            component.connects_to.remove(&op.name);
            // clone: each connection owns the name.
            component.connects_to.insert(op.to.clone());
        }
    }
    Ok(Done {
        to: Some(op.to),
        rewrote: decisions.len() + components.len(),
        ..Done::of(OpKind::RenameComponent, op.name)
    })
}

pub(super) fn remove(edit: &mut Edit<'_>, op: RemoveComponent) -> Result<Done, Fault> {
    existing(edit, &op.name, Field::Name)?;
    let (decisions, components) = uses(edit, &op.name);
    if !decisions.is_empty() || !components.is_empty() {
        return Err(Fault::op(Rule::InUse {
            decisions,
            components,
        }));
    }
    edit.take_component(&op.name);
    edit.end(&op.name);
    Ok(Done::of(OpKind::RemoveComponent, op.name))
}

/// A description meets `store::text`.
fn description(text: &str) -> Result<(), Fault> {
    match fault(TextField::Description, text) {
        Some(fault) => Err(Fault::at(
            Field::Description,
            Rule::Text {
                field: TextField::Description,
                fault,
            },
        )),
        None => Ok(()),
    }
}

/// The component `name`, given as `field`, as the graph holds it now.
fn existing<'e>(edit: &'e Edit<'_>, name: &str, field: Field) -> Result<&'e Component, Fault> {
    edit.state().components.get(name).ok_or_else(|| {
        Fault::at(
            field,
            Rule::NotFound {
                name: name.to_owned(),
                kind: NodeKind::Component,
            },
        )
    })
}

/// Why `from` may not connect to `target`, if it may not.
fn connection(edit: &Edit<'_>, from: &str, target: &str) -> Option<Rule> {
    if target == from {
        Some(Rule::SelfEdge)
    } else if target == PROJECT_COMPONENT || edit.state().components.contains_key(target) {
        None
    } else {
        Some(Rule::ConnectionMissing {
            target: target.to_owned(),
        })
    }
}

/// The decisions of the component `name` and the other components that
/// connect to it, each by name.
fn uses(edit: &Edit<'_>, name: &str) -> (Vec<String>, Vec<String>) {
    let state = edit.state();
    let decisions = state
        .decisions_of(name)
        .into_iter()
        .map(|(decision, _)| decision.to_owned())
        .collect();
    let components = state
        .connects_from(name)
        .into_iter()
        .filter(|&component| component != name)
        .map(str::to_owned)
        .collect();
    (decisions, components)
}
