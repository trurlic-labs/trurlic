//! The budgets of work a file's index may take, each linear in the file:
//! a rule that would run superlinear on some input is stopped at a bound
//! every machine reaches alike, and the file is refused as
//! [`IndexFault::TooComplex`], rather than found by its time.
//!
//! Three budgets, each counted in its own unit: the parse (`syntax.rs`:
//! parser steps, and bytes the lexer is served), the scans a resolver
//! runs over the tree or text after it, and the definitions a macro
//! expansion adds (`expand.rs`). A tree-sitter span hash is no part of
//! any: each token is hashed once by construction (`syntax.rs`).

use super::index::IndexFault;
use super::scope::MAX_NESTING;

/// Parser steps, and bytes the lexer is served, per byte of the file.
/// Source a person wrote takes about one of each; the bound leaves a wide
/// margin for code mid-edit.
pub(super) const PARSE_WORK_PER_BYTE: usize = 64;

/// Tokens a tree-sitter resolver's lexical rules read per byte of the
/// file (the token trees of macros, read at most four times each).
pub(super) const SCAN_WORK_PER_BYTE: usize = 16;

/// Bytes a lexical resolver hashes per byte of the file: a byte is
/// hashed once per definition whose span holds it, and spans nest at
/// most [`MAX_NESTING`] deep along a path; twice that leaves room for
/// spans that overlap without nesting (a TOML table and the one whose
/// header comes between its own and its sub-table's).
pub(super) const LEXICAL_WORK_PER_BYTE: usize = 2 * MAX_NESTING;

/// Definitions a macro expansion may add per byte of the invocations
/// that add them.
pub(super) const EXPANSION_WORK_PER_BYTE: usize = 4;

/// The work any file may take, however short, in each budget but the
/// expansion's.
const MIN_WORK: usize = 1 << 20;

/// The definitions any file's invocations may add, however short: room
/// for a macro of many members invoked a few times.
const MIN_EXPANSION: usize = 1 << 16;

/// What is left of one budget of a file.
#[derive(Debug)]
pub(super) struct Work {
    left: usize,
}

impl Work {
    /// A budget of `units`.
    pub(super) const fn units(left: usize) -> Self {
        Self { left }
    }

    /// The budget of `per_byte` units for each of `bytes`, and
    /// [`MIN_WORK`] at least.
    pub(super) fn of(bytes: usize, per_byte: usize) -> Self {
        Self::units(bytes.saturating_mul(per_byte).max(MIN_WORK))
    }

    /// The expansion's budget for invocations of `bytes` bytes.
    pub(super) fn expansion(bytes: usize) -> Self {
        Self::units(
            bytes
                .saturating_mul(EXPANSION_WORK_PER_BYTE)
                .max(MIN_EXPANSION),
        )
    }

    /// Spend `units`; [`IndexFault::TooComplex`] once the budget is gone.
    pub(super) fn spend(&mut self, units: usize) -> Result<(), IndexFault> {
        self.left = self.left.checked_sub(units).ok_or(IndexFault::TooComplex)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_is_linear_in_the_file_and_refuses_past_it() {
        let bytes = MIN_WORK;
        let mut work = Work::of(bytes, SCAN_WORK_PER_BYTE);

        assert_eq!(work.spend(bytes * SCAN_WORK_PER_BYTE), Ok(()));
        assert_eq!(work.spend(1), Err(IndexFault::TooComplex));
        assert!(Work::of(0, SCAN_WORK_PER_BYTE).spend(MIN_WORK).is_ok());
        assert_eq!(
            Work::expansion(0).spend(MIN_EXPANSION + 1),
            Err(IndexFault::TooComplex)
        );
    }
}
