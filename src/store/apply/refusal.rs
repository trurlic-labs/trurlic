//! Why the store refused a batch: the op at fault, the argument it
//! names, and the rule it breaks. A batch is refused at its first fault,
//! so a refusal names one op.

use std::fmt;

use super::ops::{Field, OpKind};
use crate::store::name::NameFault;
use crate::store::quality::{Broken, QualityFault};
use crate::store::schema::AnchorFault;
use crate::store::state::NodeFile;
use crate::store::text::{TextFault, TextField};
use crate::store::validate::Issue;

/// How many names a refusal lists before it counts the rest.
const LISTED_NAMES: usize = 10;

/// A refused batch: nothing of it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The op at fault; `None` when the fault is the batch's own: an
    /// empty batch, its origin or session.
    pub op: Option<OpAt>,
    /// The argument at fault, within the op (`add_tags[0]`) or the batch
    /// (`origin`). A rule of the quality bar on one element names the
    /// argument that brought it, by its index there; a rule on what no one
    /// argument holds (a count of a whole list, a merge's union, an element
    /// the decision held already) names the decision's field without an
    /// index. `None` when the op as a whole breaks the rule.
    pub field: Option<String>,
    pub rule: Rule,
}

impl Refusal {
    /// Where the fault is in the batch's arguments: `ops[3].add_tags[0]`,
    /// `ops[3]`, `origin`.
    #[must_use]
    pub fn path(&self) -> String {
        let op = self.op.as_ref().map(|op| op.index);
        argument_path(op, self.field.as_deref())
    }
}

/// `field` within the op at index `op`, or within the batch for `None`, as
/// a refusal and a warning name it: `ops[3].add_tags[0]`, `ops[3]` for the
/// op as a whole, `origin`, and `ops` for the batch as a whole.
pub(super) fn argument_path(op: Option<usize>, field: Option<&str>) -> String {
    match (op, field) {
        (Some(index), Some(field)) => format!("ops[{index}].{field}"),
        (Some(index), None) => format!("ops[{index}]"),
        (None, Some(field)) => field.to_owned(),
        (None, None) => "ops".to_owned(),
    }
}

/// An op of a batch: its index, kind and the node it names first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpAt {
    pub index: usize,
    pub kind: OpKind,
    pub target: String,
}

/// The kind of node a rule names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Component,
    Decision,
}

impl NodeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Decision => "decision",
        }
    }
}

/// The rule a refused op breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// The batch holds no op.
    EmptyBatch,
    Text {
        field: TextField,
        fault: TextFault,
    },
    Anchor(AnchorFault),
    Name(NameFault),
    /// A node of either kind holds the name already.
    NameTaken {
        name: String,
        holder: NodeKind,
    },
    NotFound {
        name: String,
        kind: NodeKind,
    },
    ComponentMissing {
        component: String,
    },
    ConnectionMissing {
        target: String,
    },
    DependencyMissing {
        target: String,
    },
    /// An edge from a node to itself.
    SelfEdge,
    /// A `depends_on` cycle the graph did not have, by its sorted members.
    Cycle {
        members: Vec<String>,
    },
    /// The op leaves every file as it is.
    NoChange,
    /// A merge into itself, or a repoint to the decision removed.
    SameNode,
    /// A decision removed while others depend on it, by name.
    Dependents {
        names: Vec<String>,
    },
    /// A component removed while decisions belong to it or components
    /// connect to it, by name.
    InUse {
        decisions: Vec<String>,
        components: Vec<String>,
    },
    /// A list given whole and by element in one op.
    BothForms {
        whole: Field,
    },
    /// A `remove_*` element the node does not hold.
    NotHeld {
        element: String,
    },
    /// A rule of the quality bar, built in or of `[quality]`.
    Quality(QualityFault),
    /// An issue the resulting graph has and the loaded one lacks, which
    /// no rule above refused.
    Issue(Issue),
    /// The canonical text of `file` the batch leaves is `bytes`, over the
    /// `max` a load reads: lists grown by `add_*` or a merge's union.
    FileTooLarge {
        file: NodeFile,
        bytes: usize,
        max: usize,
    },
}

impl From<Broken> for Rule {
    fn from(broken: Broken) -> Self {
        match broken {
            Broken::Text { field, fault } => Self::Text { field, fault },
            Broken::Quality(fault) => Self::Quality(fault),
        }
    }
}

impl Rule {
    /// The rule as a refusal names it.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EmptyBatch => "empty_batch",
            Self::Text { .. } => "invalid_text",
            Self::Anchor(_) => "invalid_anchor",
            Self::Name(NameFault::Reserved) => "name_reserved",
            Self::Name(NameFault::NotKebab | NameFault::TooLong { .. }) => "name_invalid",
            Self::NameTaken { .. } => "name_taken",
            Self::NotFound { .. } => "not_found",
            Self::ComponentMissing { .. } => "component_missing",
            Self::ConnectionMissing { .. } => "connection_missing",
            Self::DependencyMissing { .. } => "dependency_missing",
            Self::SelfEdge => "self_edge",
            Self::Cycle { .. } => "depends_on_cycle",
            Self::NoChange => "no_change",
            Self::SameNode => "same_node",
            Self::Dependents { .. } => "has_dependents",
            Self::InUse { .. } => "component_in_use",
            Self::BothForms { .. } => "both_forms",
            Self::NotHeld { .. } => "not_held",
            Self::Quality(fault) => fault.as_str(),
            Self::Issue(issue) => issue.kind.as_str(),
            Self::FileTooLarge { .. } => "file_too_large",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBatch => f.write_str("the batch holds no op"),
            Self::Text { fault, .. } => write!(f, "{fault}"),
            Self::Anchor(fault) => write!(f, "{fault}"),
            Self::Name(fault) => write!(f, "the name {fault}"),
            Self::NameTaken { name, holder } => {
                write!(f, "`{name}` is taken: a {} has that name", holder.as_str())
            }
            Self::NotFound { name, kind } => {
                write!(f, "names `{name}`, which is no {}", kind.as_str())
            }
            Self::ComponentMissing { component: target } | Self::ConnectionMissing { target } => {
                write!(f, "names `{target}`, which is no component")
            }
            Self::DependencyMissing { target } => {
                write!(f, "names `{target}`, which is no decision")
            }
            Self::SelfEdge => f.write_str("names the node the edge leaves"),
            Self::Cycle { members } => {
                write!(f, "closes a depends_on cycle among {}", listed(members))
            }
            Self::NoChange => f.write_str("changes nothing: every file would keep its bytes"),
            Self::SameNode => f.write_str("names the decision the op already names"),
            Self::Dependents { names } => write!(
                f,
                "{} decision(s) depend on it: {}; give `repoint_to`, or revise them first",
                names.len(),
                listed(names)
            ),
            Self::InUse {
                decisions,
                components,
            } => in_use(f, decisions, components),
            Self::BothForms { whole } => write!(
                f,
                "is given with `{}`, which sets the whole list; give one form",
                whole.as_str()
            ),
            Self::NotHeld { element } => {
                write!(f, "names `{element}`, which the list does not hold")
            }
            Self::Quality(fault) => write!(f, "{fault}"),
            Self::Issue(issue) => write!(f, "the graph would gain an issue: {}", issue.message),
            Self::FileTooLarge { file, bytes, max } => write!(
                f,
                "leaves `{file}` at {bytes} bytes, over the {max} a node file may hold; \
                 remove elements of its lists, or split the decision"
            ),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        located(f, self.op.as_ref(), self.field.as_deref())?;
        write!(f, "{}", self.rule)
    }
}

/// What uses a component a remove is refused for: the decisions that
/// belong to it, the components that connect to it, or both.
fn in_use(f: &mut fmt::Formatter<'_>, decisions: &[String], components: &[String]) -> fmt::Result {
    let mut uses = Vec::new();
    if !decisions.is_empty() {
        uses.push(format!("the decisions {} belong to it", listed(decisions)));
    }
    if !components.is_empty() {
        uses.push(format!(
            "the components {} connect to it",
            listed(components)
        ));
    }
    write!(f, "{}; move them first", uses.join(" and "))
}

/// The start of a refusal's message: the op and the argument it is about,
/// when it is about one.
fn located(f: &mut fmt::Formatter<'_>, op: Option<&OpAt>, field: Option<&str>) -> fmt::Result {
    if let Some(op) = op {
        write!(
            f,
            "ops[{}] ({} `{}`)",
            op.index,
            op.kind.as_str(),
            op.target
        )?;
        if field.is_some() {
            f.write_str(", ")?;
        } else {
            f.write_str(": ")?;
        }
    }
    if let Some(field) = field {
        write!(f, "`{field}`: ")?;
    }
    Ok(())
}

/// `names` in backticks, comma-separated, past [`LISTED_NAMES`] counted.
fn listed(names: &[String]) -> String {
    let mut shown: Vec<String> = names
        .iter()
        .take(LISTED_NAMES)
        .map(|name| format!("`{name}`"))
        .collect();
    if names.len() > LISTED_NAMES {
        shown.push(format!("and {} more", names.len() - LISTED_NAMES));
    }
    shown.join(", ")
}
