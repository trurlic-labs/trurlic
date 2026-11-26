//! The ops of a batch as `apply` declares them: the tag that names each
//! op, its fields with their limits, and the shapes of an anchor and a
//! repair, which the parse turns into the store's ops.

use std::collections::BTreeMap;

use crate::store::limits::{
    MAX_ALTERNATIVE_BYTES, MAX_ANCHOR_PATH_BYTES, MAX_ANCHOR_SYMBOL_BYTES, MAX_BATCH_OPS,
    MAX_CHOICE_BYTES, MAX_LIST_ITEMS, MAX_NAME_BYTES, MAX_TAG_BYTES, MAX_TEXT_FIELD_BYTES,
};
use crate::store::{
    AddComponent, AnchorInput, Field, ListEdit, Merge, Op, OpKind, Record, Remove, RemoveComponent,
    Rename, Repair, RepairAnchors, Revise, ReviseComponent,
};
use crate::tools::spec::kinds::{List, Object, Text, Union, Variant};
use crate::tools::spec::{Args, Fields, Kind};

/// The ops of a batch, each named by its `op` tag.
pub(super) static OPS: List<Union<Op>> = List {
    item: Union {
        tag: "op",
        variants: &VARIANTS,
    },
    max_items: MAX_BATCH_OPS,
};

const NAME: Text = Text {
    max_bytes: MAX_NAME_BYTES,
};
const NAMES: List<Text> = List {
    item: NAME,
    max_items: MAX_LIST_ITEMS,
};
const CHOICE: Text = Text {
    max_bytes: MAX_CHOICE_BYTES,
};
const PROSE: Text = Text {
    max_bytes: MAX_TEXT_FIELD_BYTES,
};
const ALTERNATIVES: List<Text> = List {
    item: Text {
        max_bytes: MAX_ALTERNATIVE_BYTES,
    },
    max_items: MAX_LIST_ITEMS,
};
const TAGS: List<Text> = List {
    item: Text {
        max_bytes: MAX_TAG_BYTES,
    },
    max_items: MAX_LIST_ITEMS,
};
const ANCHORS: List<Object<AnchorInput>> = List {
    item: Object::new(),
    max_items: MAX_LIST_ITEMS,
};

static VARIANTS: [Variant<Op>; 10] = [
    variant(OpKind::Record, record),
    variant(OpKind::Revise, revise),
    variant(OpKind::Rename, |fields| Op::Rename(rename(fields))),
    variant(OpKind::Merge, merge),
    variant(OpKind::Remove, remove),
    variant(OpKind::AddComponent, add_component),
    variant(OpKind::ReviseComponent, revise_component),
    variant(OpKind::RenameComponent, |fields| {
        Op::RenameComponent(rename(fields))
    }),
    variant(OpKind::RemoveComponent, |fields| {
        Op::RemoveComponent(RemoveComponent {
            name: fields.required(Field::Name.as_str(), "The component.", &NAME),
        })
    }),
    variant(OpKind::RepairAnchors, repair_anchors),
];

const fn variant(kind: OpKind, declare: fn(&mut Fields<'_, '_>) -> Op) -> Variant<Op> {
    Variant {
        name: kind.as_str(),
        declare,
    }
}

fn record(fields: &mut Fields<'_, '_>) -> Op {
    Op::Record(Record {
        name: fields.required(
            Field::Name.as_str(),
            "The new decision's name, its file stem: kebab-case, unique across nodes.",
            &NAME,
        ),
        component: fields.required(
            Field::Component.as_str(),
            "Its component, or `project` for a rule that holds everywhere.",
            &NAME,
        ),
        choice: fields.required(Field::Choice.as_str(), "The rule, one line.", &CHOICE),
        reason: fields.required(Field::Reason.as_str(), "Why it holds.", &PROSE),
        alternatives: list(
            fields,
            Field::Alternatives,
            "Rejected options, one line each.",
            &ALTERNATIVES,
        ),
        tags: list(fields, Field::Tags, "Labels, one line each.", &TAGS),
        anchors: list(fields, Field::Anchors, "Where it applies.", &ANCHORS),
        depends_on: list(fields, Field::DependsOn, "Decisions it depends on.", &NAMES),
    })
}

fn revise(fields: &mut Fields<'_, '_>) -> Op {
    Op::Revise(Revise {
        name: fields.required(Field::Name.as_str(), "The decision.", &NAME),
        component: fields.optional(Field::Component.as_str(), "Its new component.", &NAME),
        choice: fields.optional(Field::Choice.as_str(), "The new rule, one line.", &CHOICE),
        reason: fields.optional(Field::Reason.as_str(), "The new reason.", &PROSE),
        alternatives: edit(fields, Field::Alternatives, &ALTERNATIVES),
        tags: edit(fields, Field::Tags, &TAGS),
        anchors: edit(fields, Field::Anchors, &ANCHORS),
        depends_on: edit(fields, Field::DependsOn, &NAMES),
    })
}

fn rename(fields: &mut Fields<'_, '_>) -> Rename {
    Rename {
        name: fields.required(Field::Name.as_str(), "The node.", &NAME),
        to: fields.required(Field::To.as_str(), "Its new name.", &NAME),
    }
}

fn merge(fields: &mut Fields<'_, '_>) -> Op {
    Op::Merge(Merge {
        survivor: fields.required(Field::Survivor.as_str(), "The decision kept.", &NAME),
        absorbed: fields.required(
            Field::Absorbed.as_str(),
            "The decision folded into it and removed.",
            &NAME,
        ),
    })
}

fn remove(fields: &mut Fields<'_, '_>) -> Op {
    Op::Remove(Remove {
        name: fields.required(Field::Name.as_str(), "The decision.", &NAME),
        repoint_to: fields.optional(
            Field::RepointTo.as_str(),
            "The decision each edge to the removed one points at instead.",
            &NAME,
        ),
    })
}

fn add_component(fields: &mut Fields<'_, '_>) -> Op {
    Op::AddComponent(AddComponent {
        name: fields.required(
            Field::Name.as_str(),
            "The new component's name, its file stem.",
            &NAME,
        ),
        description: fields.required(Field::Description.as_str(), "What it is.", &PROSE),
        connects_to: list(
            fields,
            Field::ConnectsTo,
            "Components it connects to.",
            &NAMES,
        ),
    })
}

fn revise_component(fields: &mut Fields<'_, '_>) -> Op {
    Op::ReviseComponent(ReviseComponent {
        name: fields.required(Field::Name.as_str(), "The component.", &NAME),
        description: fields.optional(Field::Description.as_str(), "Its new description.", &PROSE),
        connects_to: edit(fields, Field::ConnectsTo, &NAMES),
    })
}

const REPAIRS: List<Object<Repair>> = List {
    item: Object::new(),
    max_items: MAX_LIST_ITEMS,
};

/// The repairs given, or, when none are, every repair the tree shows,
/// which [`prepare`] fills in.
fn repair_anchors(fields: &mut Fields<'_, '_>) -> Op {
    let about = "Each stale anchor to replace, in its decision; absent, every repair \
                 `trurlic check` shows, in each decision ever accepted that holds the stale \
                 anchor when the op runs.";
    match fields.optional(Field::Repairs.as_str(), about, &REPAIRS) {
        Some(repairs) => Op::RepairAnchors(RepairAnchors::Listed(repairs)),
        None => Op::RepairAnchors(RepairAnchors::Every(BTreeMap::new())),
    }
}

/// A repair as a batch writes it.
impl Args for Repair {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        let anchor = Object::<AnchorInput>::new();
        Self {
            decision: fields.required("decision", "The decision that holds the anchor.", &NAME),
            from: fields.required("from", "The stale anchor it holds.", &anchor),
            to: fields.required(
                "to",
                "The anchor it holds instead; when it holds this one already, the stale \
                 one is dropped.",
                &anchor,
            ),
        }
    }
}

/// An optional list field, empty when absent.
fn list<K: Kind>(
    fields: &mut Fields<'_, '_>,
    field: Field,
    description: &'static str,
    kind: &List<K>,
) -> Vec<K::Out> {
    fields
        .optional(field.as_str(), description, kind)
        .unwrap_or_default()
}

/// A list field and its `add_` and `remove_` fields.
fn edit<K: Kind>(fields: &mut Fields<'_, '_>, field: Field, kind: &List<K>) -> ListEdit<K::Out> {
    let Some((add, remove)) = field.by_element() else {
        return ListEdit::keep();
    };
    ListEdit {
        whole: fields.optional(field.as_str(), "The whole list, replacing it.", kind),
        add: fields.optional(add.as_str(), "Elements to add.", kind),
        remove: fields.optional(
            remove.as_str(),
            "Elements to remove; each must be in the list.",
            kind,
        ),
    }
}

/// An anchor as a batch writes it.
impl Args for AnchorInput {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        Self {
            file: fields.required(
                "file",
                "A file or directory, relative to the project root, `/`-separated.",
                &Text {
                    max_bytes: MAX_ANCHOR_PATH_BYTES,
                },
            ),
            symbol: fields.optional(
                "symbol",
                "A symbol in the file, its `::`-separated path.",
                &Text {
                    max_bytes: MAX_ANCHOR_SYMBOL_BYTES,
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::tools::spec::At;

    fn ops(arguments: &Value) -> Vec<Op> {
        OPS.parse(&arguments["ops"], At::Root).unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|&item| item.to_owned()).collect()
    }

    /// Each op's every field reaches the store's op under its own name.
    #[test]
    fn each_op_parses_into_the_store_op_it_names() {
        let anchor = |file: &str| AnchorInput {
            file: file.into(),
            symbol: None,
        };
        let parsed = ops(&json!({ "ops": [
            {
                "op": "record", "name": "a", "component": "c", "choice": "C", "reason": "R",
                "alternatives": ["x"], "tags": ["t"], "anchors": [{ "file": "f" }],
                "depends_on": ["d"],
            },
            {
                "op": "revise", "name": "a", "component": "c", "choice": "C", "reason": "R",
                "alternatives": ["x"], "add_tags": ["t"], "remove_tags": ["u"],
                "remove_anchors": [{ "file": "f" }], "add_depends_on": ["d"],
            },
            { "op": "rename", "name": "a", "to": "b" },
            { "op": "merge", "survivor": "a", "absorbed": "b" },
            { "op": "remove", "name": "a", "repoint_to": "b" },
            { "op": "add_component", "name": "c", "description": "D", "connects_to": ["e"] },
            { "op": "revise_component", "name": "c", "description": "D", "remove_connects_to": ["e"] },
            { "op": "rename_component", "name": "c", "to": "e" },
            { "op": "remove_component", "name": "c" },
        ]}));
        assert_eq!(
            parsed,
            [
                Op::Record(Record {
                    name: "a".into(),
                    component: "c".into(),
                    choice: "C".into(),
                    reason: "R".into(),
                    alternatives: strings(&["x"]),
                    tags: strings(&["t"]),
                    anchors: vec![anchor("f")],
                    depends_on: strings(&["d"]),
                }),
                Op::Revise(Revise {
                    name: "a".into(),
                    component: Some("c".into()),
                    choice: Some("C".into()),
                    reason: Some("R".into()),
                    alternatives: ListEdit {
                        whole: Some(strings(&["x"])),
                        ..ListEdit::keep()
                    },
                    tags: ListEdit {
                        add: Some(strings(&["t"])),
                        remove: Some(strings(&["u"])),
                        ..ListEdit::keep()
                    },
                    anchors: ListEdit {
                        remove: Some(vec![anchor("f")]),
                        ..ListEdit::keep()
                    },
                    depends_on: ListEdit {
                        add: Some(strings(&["d"])),
                        ..ListEdit::keep()
                    },
                }),
                Op::Rename(Rename {
                    name: "a".into(),
                    to: "b".into()
                }),
                Op::Merge(Merge {
                    survivor: "a".into(),
                    absorbed: "b".into()
                }),
                Op::Remove(Remove {
                    name: "a".into(),
                    repoint_to: Some("b".into())
                }),
                Op::AddComponent(AddComponent {
                    name: "c".into(),
                    description: "D".into(),
                    connects_to: strings(&["e"]),
                }),
                Op::ReviseComponent(ReviseComponent {
                    name: "c".into(),
                    description: Some("D".into()),
                    connects_to: ListEdit {
                        remove: Some(strings(&["e"])),
                        ..ListEdit::keep()
                    },
                }),
                Op::RenameComponent(Rename {
                    name: "c".into(),
                    to: "e".into()
                }),
                Op::RemoveComponent(RemoveComponent { name: "c".into() }),
            ]
        );
    }
}
