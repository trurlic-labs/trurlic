//! The checks the ops share, each refusing with the argument at fault:
//! a new name, the quality bar of a decision, an anchor, and an edit of a
//! list.

use std::collections::BTreeSet;
use std::fmt::Display;

use super::decision::Context;
use super::edit::Edit;
use super::ops::{AnchorInput, Field, ListEdit};
use super::refusal::{NodeKind, Rule};
use crate::store::quality::DecisionField;
use crate::store::schema::{Anchor, Decision};
use crate::store::state::name_fault;

/// What an op breaks: the argument within it, `None` for the op as a
/// whole, and the rule.
#[derive(Debug)]
pub(super) struct Fault {
    pub(super) field: Option<String>,
    pub(super) rule: Rule,
}

impl Fault {
    pub(super) const fn op(rule: Rule) -> Self {
        Self { field: None, rule }
    }

    pub(super) fn at(field: Field, rule: Rule) -> Self {
        Self {
            field: Some(field.as_str().to_owned()),
            rule,
        }
    }

    pub(super) fn item(field: Field, index: usize, rule: Rule) -> Self {
        Self {
            field: Some(format!("{}[{index}]", field.as_str())),
            rule,
        }
    }
}

/// `name`, given as `field`, is a free name: a valid one no node holds.
pub(super) fn new_name(edit: &Edit<'_>, name: &str, field: Field) -> Result<(), Fault> {
    if let Some(fault) = name_fault(name) {
        return Err(Fault::at(field, Rule::Name(fault)));
    }
    let holder = if edit.state().decisions.contains_key(name) {
        NodeKind::Decision
    } else if edit.state().components.contains_key(name) {
        NodeKind::Component
    } else {
        return Ok(());
    };
    Err(Fault::at(
        field,
        Rule::NameTaken {
            name: name.to_owned(),
            holder,
        },
    ))
}

/// `decision` meets the bar in the fields of `held`, refused naming the
/// decision's field at fault; the warnings join the op's.
pub(super) fn bar(
    context: &mut Context<'_>,
    decision: &Decision,
    held: &[DecisionField],
) -> Result<(), Fault> {
    let hits = context.bar.judge(decision, held).map_err(|breach| Fault {
        field: Some(breach.place.to_string()),
        rule: breach.broken.into(),
    })?;
    context.hits.extend(hits);
    Ok(())
}

/// The fields of `changed` the bar reads.
pub(super) fn judged(changed: &[Field]) -> Vec<DecisionField> {
    changed
        .iter()
        .filter_map(|field| match field {
            Field::Choice => Some(DecisionField::Choice),
            Field::Reason => Some(DecisionField::Reason),
            Field::Alternatives => Some(DecisionField::Alternatives),
            Field::Tags => Some(DecisionField::Tags),
            Field::Anchors => Some(DecisionField::Anchors),
            // The fields no rule of the bar reads: names and edges.
            Field::Name
            | Field::To
            | Field::Survivor
            | Field::Absorbed
            | Field::RepointTo
            | Field::Component
            | Field::AddAlternatives
            | Field::RemoveAlternatives
            | Field::AddTags
            | Field::RemoveTags
            | Field::AddAnchors
            | Field::RemoveAnchors
            | Field::DependsOn
            | Field::AddDependsOn
            | Field::RemoveDependsOn
            | Field::Description
            | Field::ConnectsTo
            | Field::AddConnectsTo
            | Field::RemoveConnectsTo => None,
        })
        .collect()
}

/// `inputs`, given as `field`, as anchors.
pub(super) fn anchors(field: Field, inputs: Vec<AnchorInput>) -> Result<Vec<Anchor>, Fault> {
    inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            Anchor::new(input.file, input.symbol)
                .map_err(|fault| Fault::item(field, index, Rule::Anchor(fault)))
        })
        .collect()
}

/// Each list of an anchor edit as anchors, refused at the first input
/// that is none.
pub(super) fn anchor_edit(edit: ListEdit<AnchorInput>) -> Result<ListEdit<Anchor>, Fault> {
    let (add, remove) = by_element(Field::Anchors);
    Ok(ListEdit {
        whole: edit
            .whole
            .map(|inputs| anchors(Field::Anchors, inputs))
            .transpose()?,
        add: edit.add.map(|inputs| anchors(add, inputs)).transpose()?,
        remove: edit
            .remove
            .map(|inputs| anchors(remove, inputs))
            .transpose()?,
    })
}

/// The `add_` and `remove_` fields of the list field `field`.
fn by_element(field: Field) -> (Field, Field) {
    field.by_element().unwrap_or((field, field))
}

/// A list a node holds: a set, or the ordered alternatives.
pub(super) trait Elements<T> {
    fn holds(&self, element: &T) -> bool;
    fn put(&mut self, element: T);
    /// Remove `element`, and whether it was there.
    fn take(&mut self, element: &T) -> bool;
    fn replace(&mut self, elements: Vec<T>);
}

impl<T: Ord> Elements<T> for BTreeSet<T> {
    fn holds(&self, element: &T) -> bool {
        self.contains(element)
    }

    fn put(&mut self, element: T) {
        self.insert(element);
    }

    fn take(&mut self, element: &T) -> bool {
        self.remove(element)
    }

    fn replace(&mut self, elements: Vec<T>) {
        *self = elements.into_iter().collect();
    }
}

impl<T: PartialEq> Elements<T> for Vec<T> {
    fn holds(&self, element: &T) -> bool {
        self.contains(element)
    }

    fn put(&mut self, element: T) {
        self.push(element);
    }

    fn take(&mut self, element: &T) -> bool {
        let at = self.iter().position(|held| held == element);
        at.map(|at| self.remove(at)).is_some()
    }

    fn replace(&mut self, elements: Vec<T>) {
        *self = elements;
    }
}

/// Apply `edit` of the list field `field` to `list`: the whole list, or
/// the removals (each of an element the list holds) and then the
/// additions, an element the list holds already staying once. `admit`
/// judges each element the edit brings into the list.
pub(super) fn edit_list<T: Display, L: Elements<T>>(
    list: &mut L,
    edit: ListEdit<T>,
    field: Field,
    admit: impl Fn(&T) -> Option<Rule>,
) -> Result<(), Fault> {
    let (add_field, remove_field) = by_element(field);
    if edit.whole.is_some() {
        let both = [(add_field, &edit.add), (remove_field, &edit.remove)]
            .into_iter()
            .find_map(|(by, given)| given.as_ref().map(|_| by));
        if let Some(by) = both {
            return Err(Fault::at(by, Rule::BothForms { whole: field }));
        }
    }
    let admitted = |list: &L, index: usize, element: &T, at: Field| {
        if list.holds(element) {
            return Ok(());
        }
        admit(element).map_or(Ok(()), |rule| Err(Fault::item(at, index, rule)))
    };
    if let Some(whole) = edit.whole {
        for (index, element) in whole.iter().enumerate() {
            admitted(list, index, element, field)?;
        }
        list.replace(whole);
    }
    for (index, element) in edit.remove.into_iter().flatten().enumerate() {
        if !list.take(&element) {
            let element = element.to_string();
            return Err(Fault::item(remove_field, index, Rule::NotHeld { element }));
        }
    }
    for (index, element) in edit.add.into_iter().flatten().enumerate() {
        admitted(list, index, &element, add_field)?;
        if !list.holds(&element) {
            list.put(element);
        }
    }
    Ok(())
}
