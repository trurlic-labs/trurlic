//! The `apply` tool: its arguments declared once for the schema and the
//! parse, its result declared once for the `outputSchema` and the JSON,
//! and a handler that hands the batch to the store's one write. Every
//! rule a batch is held to is the store's.

use crate::store::limits::{
    MAX_BATCH_OPS, MAX_CHOICE_BYTES, MAX_LIST_ITEMS, MAX_NAME_BYTES, MAX_TEXT_FIELD_BYTES,
    MAX_TRAILER_BYTES,
};
use crate::store::{
    AddComponent, AnchorInput, Applied, Batch, Done, Field, ListEdit, Merge, Op, OpAt, OpKind,
    Record, Remove, RemoveComponent, Rename, Revise, ReviseComponent, StatusName, Warning,
    WarningKind, Writer,
};

use super::spec::kinds::{List, Object, Text, Union, Variant};
use super::spec::output::{Count, Items, Nested, Str, Tag};
use super::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use super::spec::{Args, Fields, Kind, Output, Shown};

pub(crate) static APPLY: ToolSpec<ApplyArgs, Applied> = ToolSpec {
    name: "apply",
    title: "Apply",
    description: DESCRIPTION,
    destructive: Destructive::Yes,
    access: Access::Write(apply),
};

const DESCRIPTION: &str = "The one write of the decision graph: a batch of ops, \
applied in order and committed together, or refused whole with nothing written. \
Each op is checked against the graph the earlier ops left, so add a component \
before its decisions and a decision before those that depend on it. A refusal \
names the first op at fault (`op`), the argument (`field`) and the `rule`.\n\
Ops (`op` tag): record a decision (always `proposed`; carries `origin`); revise \
any of a decision's fields, a list whole or by `add_*`/`remove_*` (never both for \
one list; a removed element must be held); rename (rewrites every edge to it); \
merge `absorbed` into `survivor` (edges repointed, anchors, tags and edges \
unioned, the absorbed text dropped); remove (refused while decisions depend on it, \
unless `repoint_to` names their new target); add_component, revise_component, \
rename_component (carries its decisions and connections), remove_component \
(refused while in use).\n\
A revise that changes nothing, an edge to a missing node or to itself, and an \
edge that closes a depends_on cycle are refused. Revising the choice, reason or \
alternatives of an accepted decision proposes the new text and keeps the \
accepted one; revising back to it accepts it again. Anchors, tags, edges and \
renames leave the status alone.\n\
Quality: each field a record, revise or merge changes meets the bar. Built in: a \
one-line choice of at most 200 bytes, a reason of real text, at least one \
alternative and one anchor; then the limits and patterns of `[quality]` in \
project.toml. A refusal names the field, the rule, the measured value and the \
limit; a field the op leaves as it is is not judged. A `warn` pattern writes the \
batch and lists its match in `warnings`.\n\
The result names each op's node and what it changed, never the texts.";

/// The arguments of `apply`: what a batch file holds too.
#[derive(Debug, Default)]
pub(crate) struct ApplyArgs {
    ops: Vec<Op>,
    origin: Option<String>,
}

impl Args for ApplyArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        let ops = List {
            item: Union {
                tag: "op",
                variants: &VARIANTS,
            },
            max_items: MAX_BATCH_OPS,
        };
        Self {
            ops: fields.required("ops", "The changes, applied in order.", &ops),
            origin: fields.optional(
                "origin",
                "The task or plan row behind the batch: stored on each decision it records \
                 and named in the graph's history.",
                &Text {
                    max_bytes: MAX_TRAILER_BYTES,
                },
            ),
        }
    }
}

fn apply(writer: &mut Writer<'_>, arguments: ApplyArgs) -> Outcome<Applied> {
    let batch = Batch {
        ops: arguments.ops,
        origin: arguments.origin,
        // No request names the agent's session: MCP over stdio carries
        // none, and `clientInfo` names the client program.
        session: None,
    };
    Ok(writer.apply(batch)?)
}

// -- Ops -------------------------------------------------------------------

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
const PROSES: List<Text> = List {
    item: PROSE,
    max_items: MAX_LIST_ITEMS,
};
const ANCHORS: List<Object<AnchorArgs>> = List {
    item: Object::new(),
    max_items: MAX_LIST_ITEMS,
};

static VARIANTS: [Variant<Op>; 9] = [
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
        alternatives: list(fields, Field::Alternatives, "Rejected options.", &PROSES),
        tags: list(fields, Field::Tags, "Labels.", &NAMES),
        anchors: anchors(list(fields, Field::Anchors, "Where it applies.", &ANCHORS)),
        depends_on: list(fields, Field::DependsOn, "Decisions it depends on.", &NAMES),
    })
}

fn revise(fields: &mut Fields<'_, '_>) -> Op {
    Op::Revise(Revise {
        name: fields.required(Field::Name.as_str(), "The decision.", &NAME),
        component: fields.optional(Field::Component.as_str(), "Its new component.", &NAME),
        choice: fields.optional(Field::Choice.as_str(), "The new rule, one line.", &CHOICE),
        reason: fields.optional(Field::Reason.as_str(), "The new reason.", &PROSE),
        alternatives: edit(fields, Field::Alternatives, &PROSES),
        tags: edit(fields, Field::Tags, &NAMES),
        anchors: anchor_edit(edit(fields, Field::Anchors, &ANCHORS)),
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
#[derive(Debug, Default)]
pub(crate) struct AnchorArgs {
    file: String,
    symbol: Option<String>,
}

impl Args for AnchorArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        Self {
            file: fields.required(
                "file",
                "A file or directory, relative to the project root, `/`-separated.",
                &Text {
                    max_bytes: crate::store::limits::MAX_ANCHOR_PATH_BYTES,
                },
            ),
            symbol: fields.optional(
                "symbol",
                "A symbol in the file, its `::`-separated path.",
                &Text {
                    max_bytes: crate::store::limits::MAX_ANCHOR_SYMBOL_BYTES,
                },
            ),
        }
    }
}

fn anchors(parsed: Vec<AnchorArgs>) -> Vec<AnchorInput> {
    parsed
        .into_iter()
        .map(|anchor| AnchorInput {
            file: anchor.file,
            symbol: anchor.symbol,
        })
        .collect()
}

fn anchor_edit(parsed: ListEdit<AnchorArgs>) -> ListEdit<AnchorInput> {
    ListEdit {
        whole: parsed.whole.map(anchors),
        add: parsed.add.map(anchors),
        remove: parsed.remove.map(anchors),
    }
}

// -- Result ----------------------------------------------------------------

impl Output for Applied {
    fn declare(fields: &mut Shown<'_, Self>) {
        let ops = Items {
            item: Nested::<Done>::new(),
        };
        fields.required("ops", "What each op did, in order.", &ops, |applied| {
            &applied.ops
        });
        let warnings = Items {
            item: Nested::<Warning>::new(),
        };
        let about = "What the batch wrote but a rule warns of: each `warn` pattern's first \
                     match per op, in the order of the ops.";
        fields.optional("warnings", about, &warnings, |applied| {
            (!applied.warnings.is_empty()).then_some(&applied.warnings)
        });
        fields.required(
            "written",
            "Files written, a rename's new file included.",
            &Count,
            |applied| &applied.written,
        );
        fields.required(
            "removed",
            "Files removed, a rename's old file included.",
            &Count,
            |applied| &applied.removed,
        );
    }
}

const OP_KIND: Tag<OpKind> = Tag {
    all: &OpKind::ALL,
    name: OpKind::as_str,
};
const WARNING_KIND: Tag<WarningKind> = Tag {
    all: &WarningKind::ALL,
    name: WarningKind::as_str,
};

/// A warning in the shape of a refusal: the op, the argument, the rule
/// and the message.
impl Output for Warning {
    fn declare(fields: &mut Shown<'_, Self>) {
        let op = "The op it is about; absent for the batch's `origin`.";
        fields.optional("op", op, &Nested::<OpAt>::new(), |warning| {
            warning.op.as_ref()
        });
        let field = "The argument, as `ops[2].reason` or `origin`.";
        fields.optional("field", field, &Str, |warning| warning.field.as_ref());
        fields.required("rule", "The rule's name.", &WARNING_KIND, |warning| {
            &warning.kind
        });
        let message = "What the rule says, with the match and its place.";
        fields.required("message", message, &Str, |warning| &warning.message);
    }
}

impl Output for OpAt {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("index", "The op's index in the batch.", &Count, |op| {
            &op.index
        });
        fields.required("op", "The op's kind.", &OP_KIND, |op| &op.kind);
        fields.required("name", "The node it names first.", &Str, |op| &op.target);
    }
}
const CHANGED: Items<Tag<Field>> = Items {
    item: Tag {
        all: &Field::CHANGEABLE,
        name: Field::as_str,
    },
};
const STATUS: Tag<StatusName> = Tag {
    all: &StatusName::ALL,
    name: StatusName::as_str,
};

impl Output for Done {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("op", "The op's kind.", &OP_KIND, |done| &done.op);
        let name = "The node it names first; a merge's survivor.";
        fields.required("name", name, &Str, |done| &done.name);
        fields.optional("to", "A rename's new name.", &Str, |done| done.to.as_ref());
        let absorbed = "A merge's removed decision.";
        fields.optional("absorbed", absorbed, &Str, |done| done.absorbed.as_ref());
        let heir = "Where a remove pointed the edges to its decision.";
        fields.optional("repoint_to", heir, &Str, |done| done.repoint_to.as_ref());
        let changed = "A revise's changed fields.";
        fields.optional("changed", changed, &CHANGED, |done| {
            (!done.changed.is_empty()).then_some(&done.changed)
        });
        let status = "The decision's new status, when the op changed it.";
        fields.optional("status", status, &STATUS, |done| done.status.as_ref());
        let rewrote = "Files of other nodes it rewrote: edges repointed, a component's \
                       decisions and connections renamed.";
        fields.optional("rewrote", rewrote, &Count, |done| {
            (done.rewrote > 0).then_some(&done.rewrote)
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::budget::MAX_TOOL_RESULT_BYTES;
    use crate::mcp::spec::{At, parse, to_value};

    fn ops(arguments: &Value) -> Vec<Op> {
        parse::<ApplyArgs>(arguments.as_object().unwrap(), At::Root)
            .unwrap()
            .ops
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

    #[test]
    fn a_batch_over_its_limit_is_refused_by_the_spec() {
        let one = json!({ "op": "remove", "name": "a" });
        let too_many = json!({ "ops": vec![one; MAX_BATCH_OPS + 1] });
        let refused = parse::<ApplyArgs>(too_many.as_object().unwrap(), At::Root).unwrap_err();
        assert_eq!(refused.field, "ops");
    }

    /// The longest result a batch can have, every op the one whose entry
    /// is longest, with the longest names and the most files rewritten,
    /// fits the result budget whole.
    #[test]
    fn the_longest_result_of_a_full_batch_fits_the_budget() {
        let name = || "n".repeat(MAX_NAME_BYTES);
        let longest = OpKind::ALL
            .into_iter()
            .map(|op| {
                let mut done = Done {
                    op,
                    name: name(),
                    to: None,
                    absorbed: None,
                    repoint_to: None,
                    changed: Vec::new(),
                    status: None,
                    rewrote: usize::MAX,
                };
                match op {
                    OpKind::Rename | OpKind::RenameComponent => done.to = Some(name()),
                    OpKind::Merge => done.absorbed = Some(name()),
                    OpKind::Remove => done.repoint_to = Some(name()),
                    OpKind::Revise => {
                        done.changed = Field::CHANGEABLE[..7].to_vec();
                        done.status = Some(StatusName::Proposed);
                    }
                    OpKind::ReviseComponent => done.changed = Field::CHANGEABLE[7..].to_vec(),
                    OpKind::Record | OpKind::AddComponent | OpKind::RemoveComponent => {}
                }
                done
            })
            .max_by_key(|done| to_value(done).to_string().len())
            .unwrap();
        let applied = Applied {
            ops: vec![longest; MAX_BATCH_OPS],
            warnings: Vec::new(),
            written: usize::MAX,
            removed: usize::MAX,
        };
        let bytes = to_value(&applied).to_string().len();
        assert!(bytes <= MAX_TOOL_RESULT_BYTES, "{bytes} bytes");
    }
}
