//! What a batch that went through reports beside its ops: the matches of
//! the `warn` patterns, each located as a refusal is, so a later kind of
//! warning takes the same shape.

use std::fmt;

use super::refusal::{OpAt, located, path};
use crate::store::quality::Hit;

/// A warning of an applied batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The op it is about; `None` for the batch's own `origin`.
    pub op: Option<OpAt>,
    /// The argument path, as a refusal's: `ops[2].reason`, `origin`.
    pub field: Option<String>,
    pub kind: WarningKind,
    /// What it says, the op and the field first, as a refusal's message.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    /// A `warn` pattern of `[quality]` matched.
    Pattern,
}

impl WarningKind {
    pub const ALL: [Self; 1] = [Self::Pattern];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
        }
    }
}

impl Warning {
    /// The warning of `hit`, a match within the op `op` or the batch.
    pub(super) fn of(op: Option<OpAt>, hit: &Hit) -> Self {
        let place = hit.place.to_string();
        let message = Message {
            op: op.as_ref(),
            field: &place,
            hit,
        }
        .to_string();
        Self {
            field: path(op.as_ref(), Some(&place)),
            op,
            kind: WarningKind::Pattern,
            message,
        }
    }
}

struct Message<'h> {
    op: Option<&'h OpAt>,
    field: &'h str,
    hit: &'h Hit,
}

impl fmt::Display for Message<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        located(f, self.op, Some(self.field))?;
        write!(f, "{}", self.hit.found)
    }
}
