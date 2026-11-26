//! An argument the spec refuses: where it sits in a call's arguments and
//! why, as a tool refusal names it.

use std::fmt;

use crate::tools::result::ToolError;

/// Where a value sits in a call's arguments, rendered as `ops[2].name`.
#[derive(Clone, Copy)]
pub(in crate::tools) enum At<'a> {
    Root,
    Field(&'a At<'a>, &'a str),
    Item(&'a At<'a>, usize),
}

impl fmt::Display for At<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => Ok(()),
            Self::Field(Self::Root, name) => f.write_str(name),
            Self::Field(parent, name) => write!(f, "{parent}.{name}"),
            Self::Item(parent, index) => write!(f, "{parent}[{index}]"),
        }
    }
}

/// An argument the spec refuses, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tools) struct InvalidArgument {
    /// The argument's path, `ops[2].name`.
    pub field: String,
    pub fault: Fault,
}

impl InvalidArgument {
    pub(super) fn new(at: At<'_>, fault: Fault) -> Self {
        Self {
            field: at.to_string(),
            fault,
        }
    }
}

impl fmt::Display for InvalidArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.field, self.fault)
    }
}

impl From<InvalidArgument> for ToolError {
    fn from(invalid: InvalidArgument) -> Self {
        Self::invalid(&invalid.field, invalid.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tools) enum Fault {
    Unknown,
    Missing,
    /// Of another JSON type; names the one the spec holds, "a string".
    Expected(&'static str),
    TooLong {
        bytes: usize,
        max_bytes: usize,
    },
    TooMany {
        items: usize,
        max_items: usize,
    },
    NotOneOf(Vec<&'static str>),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("is not declared by this tool"),
            Self::Missing => f.write_str("is required"),
            Self::Expected(kind) => write!(f, "must be {kind}"),
            Self::TooLong { bytes, max_bytes } => {
                write!(f, "is {bytes} bytes, over the limit of {max_bytes}")
            }
            Self::TooMany { items, max_items } => {
                write!(f, "has {items} items, over the limit of {max_items}")
            }
            Self::NotOneOf(names) => write!(f, "must be one of: {}", names.join(", ")),
        }
    }
}
