//! What a batch asks for: the ops of `apply`, as a surface parses them,
//! with their values unchecked. [`Field`] names each argument as the batch
//! spells it, for the surface that declares it and the refusal that cites it.

/// The ops of one `apply` call, applied in order, all or none, and the
/// trailers of its history commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub ops: Vec<Op>,
    /// The task or plan row the batch belongs to: stored on each decision
    /// it records, and the `Origin:` trailer of its commit.
    pub origin: Option<String>,
    /// The `Session:` trailer of its commit.
    pub session: Option<String>,
}

/// One change of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Record(Record),
    Revise(Revise),
    Rename(Rename),
    Merge(Merge),
    Remove(Remove),
    AddComponent(AddComponent),
    ReviseComponent(ReviseComponent),
    RenameComponent(Rename),
    RemoveComponent(RemoveComponent),
}

impl Op {
    #[must_use]
    pub const fn kind(&self) -> OpKind {
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
        }
    }

    /// The node the op names first: the decision or component it changes,
    /// a merge's survivor.
    #[must_use]
    pub fn target(&self) -> &str {
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
        }
    }
}

/// An op's kind, as the batch's `op` tag names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpKind {
    Record,
    Revise,
    Rename,
    Merge,
    Remove,
    AddComponent,
    ReviseComponent,
    RenameComponent,
    RemoveComponent,
}

impl OpKind {
    pub const ALL: [Self; 9] = [
        Self::Record,
        Self::Revise,
        Self::Rename,
        Self::Merge,
        Self::Remove,
        Self::AddComponent,
        Self::ReviseComponent,
        Self::RenameComponent,
        Self::RemoveComponent,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
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
        }
    }
}

/// A new decision, `proposed`, carrying the batch's origin and instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub component: String,
    pub choice: String,
    pub reason: String,
    pub alternatives: Vec<String>,
    pub tags: Vec<String>,
    pub anchors: Vec<AnchorInput>,
    pub depends_on: Vec<String>,
}

/// An anchor as a batch writes it, checked by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorInput {
    pub file: String,
    pub symbol: Option<String>,
}

/// New values for the fields of a decision; a field left `None` and a
/// list edit left empty keep what the decision holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revise {
    pub name: String,
    pub component: Option<String>,
    pub choice: Option<String>,
    pub reason: Option<String>,
    pub alternatives: ListEdit<String>,
    pub tags: ListEdit<String>,
    pub anchors: ListEdit<AnchorInput>,
    pub depends_on: ListEdit<String>,
}

/// A change of one list: the whole list, or the elements to remove and
/// to add. The store refuses `whole` beside either of the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEdit<T> {
    pub whole: Option<Vec<T>>,
    pub add: Option<Vec<T>>,
    pub remove: Option<Vec<T>>,
}

impl<T> ListEdit<T> {
    /// No change.
    #[must_use]
    pub const fn keep() -> Self {
        Self {
            whole: None,
            add: None,
            remove: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub name: String,
    pub to: String,
}

/// `absorbed` folded into `survivor`, which keeps its own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub survivor: String,
    pub absorbed: String,
}

/// A decision removed; with `repoint_to`, each edge that pointed at it
/// points there instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remove {
    pub name: String,
    pub repoint_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddComponent {
    pub name: String,
    pub description: String,
    pub connects_to: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviseComponent {
    pub name: String,
    pub description: Option<String>,
    pub connects_to: ListEdit<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveComponent {
    pub name: String,
}

/// An argument of an op, by the key a batch spells it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
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
}

impl Field {
    /// The fields a revise reports as changed, in the order of the file's
    /// keys.
    pub const CHANGEABLE: [Self; 9] = [
        Self::Component,
        Self::Choice,
        Self::Reason,
        Self::Alternatives,
        Self::Tags,
        Self::Anchors,
        Self::DependsOn,
        Self::Description,
        Self::ConnectsTo,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
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
        }
    }

    /// The `add_` and `remove_` fields of a list field, which edit it by
    /// element.
    #[must_use]
    pub const fn by_element(self) -> Option<(Self, Self)> {
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
            | Self::RemoveConnectsTo => None,
        }
    }
}
