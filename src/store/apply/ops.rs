//! What a batch asks for: the ops of `apply`, as a surface parses them,
//! with their values unchecked. [`Field`] names each argument as the batch
//! spells it, for the surface that declares it and the refusal that cites it.

use std::collections::BTreeMap;

use crate::store::model::Anchor;

/// The ops of one `apply` call, applied in order, all or none, and the
/// trailers of its history commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Batch {
    pub(crate) ops: Vec<Op>,
    /// The task or plan row the batch belongs to: stored on each decision
    /// it records, and the `Origin:` trailer of its commit.
    pub(crate) origin: Option<String>,
    /// The `Session:` trailer of its commit.
    pub(crate) session: Option<String>,
}

/// One change of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    Record(Record),
    Revise(Revise),
    Rename(Rename),
    Merge(Merge),
    Remove(Remove),
    AddComponent(AddComponent),
    ReviseComponent(ReviseComponent),
    RenameComponent(Rename),
    RemoveComponent(RemoveComponent),
    RepairAnchors(RepairAnchors),
}

impl Op {
    #[must_use]
    pub(super) const fn kind(&self) -> OpKind {
        match self {
            Self::Record(_) => OpKind::Record,
            Self::Revise(_) => OpKind::Revise,
            Self::Rename(_) => OpKind::Rename,
            Self::Merge(_) => OpKind::Merge,
            Self::Remove(_) => OpKind::Remove,
            Self::AddComponent(_) => OpKind::AddComponent,
            Self::ReviseComponent(_) => OpKind::ReviseComponent,
            Self::RenameComponent(_) => OpKind::RenameComponent,
            Self::RemoveComponent(_) => OpKind::RemoveComponent,
            Self::RepairAnchors(_) => OpKind::RepairAnchors,
        }
    }

    /// The node the op names first: the decision or component it changes,
    /// a merge's survivor, a repair's first decision (none for no repair).
    #[must_use]
    pub(super) fn target(&self) -> &str {
        match self {
            Self::Record(Record { name, .. })
            | Self::Revise(Revise { name, .. })
            | Self::Rename(Rename { name, .. })
            | Self::RenameComponent(Rename { name, .. })
            | Self::Remove(Remove { name, .. })
            | Self::AddComponent(AddComponent { name, .. })
            | Self::ReviseComponent(ReviseComponent { name, .. })
            | Self::RemoveComponent(RemoveComponent { name }) => name,
            Self::Merge(Merge { survivor, .. }) => survivor,
            Self::RepairAnchors(RepairAnchors::Listed(repairs)) => repairs
                .first()
                .map_or("", |repair| repair.decision.as_str()),
            Self::RepairAnchors(RepairAnchors::Every(_)) => "",
        }
    }
}

/// An op's kind, as the batch's `op` tag names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OpKind {
    Record,
    Revise,
    Rename,
    Merge,
    Remove,
    AddComponent,
    ReviseComponent,
    RenameComponent,
    RemoveComponent,
    RepairAnchors,
}

impl OpKind {
    pub(crate) const ALL: [Self; 10] = [
        Self::Record,
        Self::Revise,
        Self::Rename,
        Self::Merge,
        Self::Remove,
        Self::AddComponent,
        Self::ReviseComponent,
        Self::RenameComponent,
        Self::RemoveComponent,
        Self::RepairAnchors,
    ];

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Revise => "revise",
            Self::Rename => "rename",
            Self::Merge => "merge",
            Self::Remove => "remove",
            Self::AddComponent => "add_component",
            Self::ReviseComponent => "revise_component",
            Self::RenameComponent => "rename_component",
            Self::RemoveComponent => "remove_component",
            Self::RepairAnchors => "repair_anchors",
        }
    }
}

/// A new decision, `proposed`, carrying the batch's origin and instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) name: String,
    pub(crate) component: String,
    pub(crate) choice: String,
    pub(crate) reason: String,
    pub(crate) alternatives: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) anchors: Vec<AnchorInput>,
    pub(crate) depends_on: Vec<String>,
}

/// An anchor as a batch writes it, checked by the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AnchorInput {
    pub(crate) file: String,
    pub(crate) symbol: Option<String>,
}

/// New values for the fields of a decision; a field left `None` and a
/// list edit left empty keep what the decision holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Revise {
    pub(crate) name: String,
    pub(crate) component: Option<String>,
    pub(crate) choice: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) alternatives: ListEdit<String>,
    pub(crate) tags: ListEdit<String>,
    pub(crate) anchors: ListEdit<AnchorInput>,
    pub(crate) depends_on: ListEdit<String>,
}

/// A change of one list: the whole list, or the elements to remove and
/// to add. The store refuses `whole` beside either of the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListEdit<T> {
    pub(crate) whole: Option<Vec<T>>,
    pub(crate) add: Option<Vec<T>>,
    pub(crate) remove: Option<Vec<T>>,
}

impl<T> ListEdit<T> {
    /// No change.
    #[must_use]
    pub(crate) const fn keep() -> Self {
        Self {
            whole: None,
            add: None,
            remove: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rename {
    pub(crate) name: String,
    pub(crate) to: String,
}

/// `absorbed` folded into `survivor`, which keeps its own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Merge {
    pub(crate) survivor: String,
    pub(crate) absorbed: String,
}

/// A decision removed; with `repoint_to`, each edge that pointed at it
/// points there instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remove {
    pub(crate) name: String,
    pub(crate) repoint_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddComponent {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) connects_to: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviseComponent {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) connects_to: ListEdit<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoveComponent {
    pub(crate) name: String,
}

/// Stale anchors replaced in their decisions. The store reads no code: a
/// surface that offers the repairs the tree shows hands them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepairAnchors {
    /// The repairs a caller lists, in order, each checked against the
    /// graph the earlier ops left: `repairs[j]` names one.
    Listed(Vec<Repair>),
    /// Each anchor a key names replaced by its value, in every decision
    /// ever accepted that holds it at the op's place in the batch; one
    /// that none holds is passed over.
    Every(BTreeMap<Anchor, Anchor>),
}

/// `from`, an anchor `decision` holds, replaced by `to`; when the decision
/// holds `to` already, `from` is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repair {
    pub(crate) decision: String,
    pub(crate) from: AnchorInput,
    pub(crate) to: AnchorInput,
}

/// An argument of an op, by the key a batch spells it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Field {
    Name,
    To,
    Survivor,
    Absorbed,
    RepointTo,
    Component,
    Choice,
    Reason,
    Alternatives,
    AddAlternatives,
    RemoveAlternatives,
    Tags,
    AddTags,
    RemoveTags,
    Anchors,
    AddAnchors,
    RemoveAnchors,
    DependsOn,
    AddDependsOn,
    RemoveDependsOn,
    Description,
    ConnectsTo,
    AddConnectsTo,
    RemoveConnectsTo,
    Repairs,
}

impl Field {
    /// The fields a `revise` of a decision reports as changed, in the order
    /// of the decision file's keys.
    pub(crate) const DECISION_CHANGEABLE: [Self; 7] = [
        Self::Component,
        Self::Choice,
        Self::Reason,
        Self::Alternatives,
        Self::Tags,
        Self::Anchors,
        Self::DependsOn,
    ];

    /// The fields a `revise_component` reports as changed, in the order of
    /// the component file's keys.
    pub(crate) const COMPONENT_CHANGEABLE: [Self; 2] = [Self::Description, Self::ConnectsTo];

    /// Every field a revise reports as changed: a decision's, then a
    /// component's.
    pub(crate) const CHANGEABLE: [Self; 9] = {
        let [
            component,
            choice,
            reason,
            alternatives,
            tags,
            anchors,
            depends_on,
        ] = Self::DECISION_CHANGEABLE;
        let [description, connects_to] = Self::COMPONENT_CHANGEABLE;
        [
            component,
            choice,
            reason,
            alternatives,
            tags,
            anchors,
            depends_on,
            description,
            connects_to,
        ]
    };

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::To => "to",
            Self::Survivor => "survivor",
            Self::Absorbed => "absorbed",
            Self::RepointTo => "repoint_to",
            Self::Component => "component",
            Self::Choice => "choice",
            Self::Reason => "reason",
            Self::Alternatives => "alternatives",
            Self::AddAlternatives => "add_alternatives",
            Self::RemoveAlternatives => "remove_alternatives",
            Self::Tags => "tags",
            Self::AddTags => "add_tags",
            Self::RemoveTags => "remove_tags",
            Self::Anchors => "anchors",
            Self::AddAnchors => "add_anchors",
            Self::RemoveAnchors => "remove_anchors",
            Self::DependsOn => "depends_on",
            Self::AddDependsOn => "add_depends_on",
            Self::RemoveDependsOn => "remove_depends_on",
            Self::Description => "description",
            Self::ConnectsTo => "connects_to",
            Self::AddConnectsTo => "add_connects_to",
            Self::RemoveConnectsTo => "remove_connects_to",
            Self::Repairs => "repairs",
        }
    }

    /// The `add_` and `remove_` fields of a list field, which edit it by
    /// element.
    #[must_use]
    pub(crate) const fn by_element(self) -> Option<(Self, Self)> {
        match self {
            Self::Alternatives => Some((Self::AddAlternatives, Self::RemoveAlternatives)),
            Self::Tags => Some((Self::AddTags, Self::RemoveTags)),
            Self::Anchors => Some((Self::AddAnchors, Self::RemoveAnchors)),
            Self::DependsOn => Some((Self::AddDependsOn, Self::RemoveDependsOn)),
            Self::ConnectsTo => Some((Self::AddConnectsTo, Self::RemoveConnectsTo)),
            // Every other field holds one value.
            Self::Name
            | Self::To
            | Self::Survivor
            | Self::Absorbed
            | Self::RepointTo
            | Self::Component
            | Self::Choice
            | Self::Reason
            | Self::AddAlternatives
            | Self::RemoveAlternatives
            | Self::AddTags
            | Self::RemoveTags
            | Self::AddAnchors
            | Self::RemoveAnchors
            | Self::AddDependsOn
            | Self::RemoveDependsOn
            | Self::Description
            | Self::AddConnectsTo
            | Self::RemoveConnectsTo
            | Self::Repairs => None,
        }
    }
}
