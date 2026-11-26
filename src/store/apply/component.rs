//! The component ops: add, revise, rename and remove. A rename carries
//! its decisions and the connections to it; a remove is refused while
//! either remains.

use std::collections::BTreeSet;

use super::Done;
use super::check::{Fault, edit_list, new_name};
use super::edit::Edit;
use super::ops::{AddComponent, Field, ListEdit, OpKind, RemoveComponent, Rename, ReviseComponent};
use super::refusal::{NodeKind, Rule};
use crate::store::model::{Component, PROJECT_COMPONENT};
use crate::store::model::{TextField, fault};

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

/// A description meets `store::model::text`.
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::store::Field;
    use crate::store::apply::*;
    use crate::store::model::NodeFile;
    use crate::store::testing::{
        add, apply, graph, named, refused, rename, sample_component, write_node,
    };

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

    /// A component's description is held to the text rule where an op gives
    /// it; a blank one is plain prose.
    #[test]
    fn a_description_with_a_hidden_character_is_refused_naming_it() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        let add = |description: &str| {
            Op::AddComponent(AddComponent {
                name: "ledger".into(),
                description: description.into(),
                connects_to: Vec::new(),
            })
        };

        let refusal = refused(&store, vec![add("Books \u{202E}skoob")]);

        assert_eq!(named(&refusal).1, "ops[0].description", "{refusal}");
        assert_eq!(refusal.rule.as_str(), "invalid_text");
        apply(&store, vec![add("")]).unwrap();
    }
}
