//! The budgets of work a file's index may take, each linear in the file.
//! A rule that would run superlinear on some input is stopped at a bound
//! every machine reaches alike, and the file is refused as
//! [`IndexFault::TooComplex`], rather than found by its time.
//!
//! There are five budgets, each counted in its own unit. The parse's
//! counts bytes the lexer is served (`syntax.rs`). The
//! scan's counts tokens a tree-sitter resolver reads after it, the
//! lexical one bytes a lexical resolver hashes. A macro expansion, which
//! no cache keeps, has two: the elements it lines up
//! (`expand/resolve.rs`) and the bytes it holds (`expand.rs`). A
//! tree-sitter span hash is no part of any: each token is hashed once by
//! construction (`syntax.rs`).

use super::index::IndexFault;
use super::scope::MAX_NESTING;

/// Bytes the lexer is served per byte of the file. Source a person wrote
/// takes about one; the bound leaves a wide margin for code mid-edit.
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

/// Bytes a macro expansion may hold per byte of the invocations that
/// add it: about two definitions, each charged its scope, its definition
/// and any name of its own.
pub(super) const EXPANSION_BYTES_PER_BYTE: usize = 256;

/// Elements a macro expansion lines up per byte of the invocations it
/// expands, each rule's opening against each invocation or hand-off that
/// may open it, and members and items it carries along a hand-off.
pub(super) const RESOLUTION_WORK_PER_BYTE: usize = 64;

/// The work any file may take, however short, in each budget but the
/// expansion's.
pub(super) const MIN_WORK: usize = 1 << 20;

/// The bytes any file's expansion may hold, however short: room for a
/// macro of many members invoked a few times.
pub(super) const MIN_EXPANSION_BYTES: usize = 8 << 20;

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

    /// The expansion's budget, in bytes it holds, for invocations of
    /// `bytes` bytes.
    pub(super) fn expansion(bytes: usize) -> Self {
        Self::units(
            bytes
                .saturating_mul(EXPANSION_BYTES_PER_BYTE)
                .max(MIN_EXPANSION_BYTES),
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

    /// Whether `work` takes `units` and then refuses one more.
    fn holds_exactly(mut work: Work, units: usize) -> bool {
        work.spend(units).is_ok() && work.spend(1) == Err(IndexFault::TooComplex)
    }

    #[test]
    fn a_budget_is_linear_in_the_file_and_one_mib_of_work_at_least() {
        assert!(holds_exactly(Work::of(100_000, 16), 1_600_000));
        assert!(holds_exactly(Work::of(10, 16), 1_048_576));
    }

    #[test]
    fn an_expansion_holds_256_bytes_a_byte_and_8_mib_at_least() {
        assert!(holds_exactly(Work::expansion(100_000), 25_600_000));
        assert!(holds_exactly(Work::expansion(10), 8_388_608));
    }
}
