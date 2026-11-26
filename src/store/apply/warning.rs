//! What a batch that went through reports beside its ops: the matches of
//! the `warn` patterns, each naming the argument it is about as a refusal
//! does and the pattern by its index, whose message the result carries
//! once. An op lists at most [`MAX_WARNINGS_PER_OP`] and counts the rest,
//! and so does the batch's `origin`, so a full batch's result fits its
//! budget whatever its warnings.

use std::collections::BTreeSet;

use super::refusal::argument_path;
use crate::store::model::limits::MAX_WARNINGS_PER_OP;
use crate::store::quality::{Bar, Hit};

/// A warning of an applied batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The index of the op it is about; `None` for the batch's `origin`.
    pub op: Option<usize>,
    /// The argument path, as a refusal's: `ops[2].reason`, `origin`.
    pub field: String,
    pub kind: WarningKind,
    /// The index of the pattern in `[quality] patterns`.
    pub pattern: usize,
    /// The byte offset of the match in the argument's text.
    pub at: usize,
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

/// The message of a pattern a listed warning names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternNote {
    /// The index of the pattern in `[quality] patterns`.
    pub index: usize,
    pub message: String,
}

/// Move the first [`MAX_WARNINGS_PER_OP`] of `hits`, the warn matches of
/// the op at `op` (or of the batch's `origin`, for `None`) in the order of
/// the patterns, to `warnings`; the count of the others.
pub(super) fn report(
    op: Option<usize>,
    hits: &mut Vec<(String, Hit)>,
    warnings: &mut Vec<Warning>,
) -> usize {
    let count = hits.len();
    let listed = hits.drain(..).take(MAX_WARNINGS_PER_OP);
    warnings.extend(listed.map(|(field, hit)| Warning {
        field: argument_path(op, Some(&field)),
        op,
        kind: WarningKind::Pattern,
        pattern: hit.pattern,
        at: hit.at,
    }));
    count.saturating_sub(MAX_WARNINGS_PER_OP)
}

/// The message of each pattern `warnings` names, by index.
pub(super) fn notes(warnings: &[Warning], bar: &Bar) -> Vec<PatternNote> {
    let named: BTreeSet<usize> = warnings.iter().map(|warning| warning.pattern).collect();
    named
        .into_iter()
        .filter_map(|index| {
            let message = bar.message(index)?.to_owned();
            Some(PatternNote { index, message })
        })
        .collect()
}
