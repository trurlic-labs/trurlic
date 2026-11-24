//! The checks the ops share, each refusing with the argument at fault:
//! a new name, the quality bar of a decision, which names the argument
//! that brought the element at fault, an anchor, and an edit of a list.

use std::collections::BTreeSet;
use std::fmt::Display;

use super::decision::Context;
use super::edit::Edit;
use super::ops::{AnchorInput, Field, ListEdit};
use super::patterns::Pending;
use super::refusal::{NodeKind, Rule};
use crate::store::name::name_fault;
use crate::store::quality::{DecisionField, Place, element};
use crate::store::schema::{Anchor, Decision};

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

    fn item(field: Field, index: usize, rule: Rule) -> Self {
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

/// `decision` meets the text rules and the limits in the fields of
/// `held`, refused naming the argument at fault as `sources` place it; it
/// is kept for the patterns, which judge it once every op has run
/// (`patterns::judge`).
pub(super) fn bar(
    context: &mut Context<'_>,
    decision: &Decision,
    held: &[DecisionField],
    sources: Sources,
) -> Result<(), Fault> {
    context
        .bar
        .judge_limits(decision, held)
        .map_err(|breach| Fault {
            field: Some(sources.path(decision, breach.place)),
            rule: breach.broken.into(),
        })?;
    if context.bar.pattern_count() > 0 {
        context.pending.push(Pending {
            op: context.op,
            // clone: later ops of the batch may change the decision before
            // the patterns judge what this op wrote.
            decision: decision.clone(),
            held: held.to_vec(),
            sources,
            about: None,
        });
    }
    Ok(())
}

/// The arguments of an op that bring elements into a decision's lists,
/// each element as the bar reads it, in the caller's order: what a verdict
/// of the bar on one element names. An element no argument of the op
/// brought (one the decision held already, or a merge's union) and a rule
/// on a whole list (a count) name the decision's field alone.
#[derive(Debug, Default)]
pub(super) struct Sources {
    arguments: Vec<(DecisionField, Field, Vec<String>)>,
}

impl Sources {
    /// `elements`, given as `argument`, bring elements into `field`.
    pub(super) fn add(&mut self, field: DecisionField, argument: Field, elements: Vec<String>) {
        self.arguments.push((field, argument, elements));
    }

    /// The list `edit` of `field` brings in, its whole list and its
    /// `add_*` list, each element as `render` gives it.
    pub(super) fn add_edit<T>(
        &mut self,
        field: DecisionField,
        argument: Field,
        edit: &ListEdit<T>,
        render: impl Fn(&T) -> String,
    ) {
        let (add, _) = by_element(argument);
        for (argument, given) in [(argument, &edit.whole), (add, &edit.add)] {
            if let Some(given) = given {
                self.add(field, argument, given.iter().map(&render).collect());
            }
        }
    }

    /// The argument path of `place` in `decision`: `add_tags[0]`, or the
    /// decision's field for a place no argument holds.
    pub(super) fn path(&self, decision: &Decision, place: Place) -> String {
        let named = element(decision, place).and_then(|element| {
            self.arguments
                .iter()
                .find_map(|(field, argument, elements)| {
                    let at = elements.iter().position(|given| *given == element)?;
                    (*field == place.field).then(|| format!("{}[{at}]", argument.as_str()))
                })
        });
        named.unwrap_or_else(|| place.field.as_str().to_owned())
    }
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
            // No rule of the bar reads these: the names an op gives, a
            // decision's component and edges, a component's fields, and the
            // `add_*` and `remove_*` arguments and a repair's list, which no
            // changed field is.
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
            | Field::RemoveConnectsTo
            | Field::Repairs => None,
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
