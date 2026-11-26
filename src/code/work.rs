//! The budgets of work a file's index may take, each linear in the file.
//! A rule that would run superlinear on some input is stopped at a bound
//! every machine reaches alike, and the file is refused as
//! [`IndexFault::TooComplex`], rather than found by its time.
//!
//! There are four budgets, each counted in its own unit. The parse has
//! two (`syntax.rs`): the bytes its lexer is served, and the runtime's
//! progress operations, which count the parser's actions, every step of a
//! walk of its stack and every 64 bytes it allocates. The scan's counts
//! the nodes the general resolver enters and the bytes its predicates read
//! after the parse, the lexical one bytes a lexical resolver hashes. A
//! tree-sitter span hash is no part of any: each token is hashed once by
//! construction (`syntax.rs`).

use super::index::IndexFault;
use super::scope::MAX_NESTING;

/// Bytes the lexer is served per byte of the file. Source a person wrote
/// takes about one; the bound leaves a wide margin for code mid-edit.
pub(super) const SERVED_BYTES_PER_BYTE: usize = 64;

/// The runtime's progress operations a parse may take per byte of its
/// file. The costliest source files measured, generated tables among
/// them, take about half of it. Since every 64 bytes allocated is one, a
/// parse allocates at most 2 KiB a byte of its file, and so holds no more.
pub(super) const PARSE_OPERATIONS_PER_BYTE: usize = 32;

/// The progress operations any parse may take, however short its file: 4
/// MiB of allocation.
pub(super) const MIN_PARSE_OPERATIONS: usize = 1 << 16;

/// Nodes the general resolver enters and bytes its predicates read per
/// byte of the file.
pub(super) const SCAN_WORK_PER_BYTE: usize = 16;

/// Bytes a lexical resolver hashes per byte of the file: a byte is
/// hashed once per definition whose span holds it, and spans nest at
/// most [`MAX_NESTING`] deep along a path; twice that leaves room for
/// spans that overlap without nesting (a TOML table and the one whose
/// header comes between its own and its sub-table's).
pub(super) const LEXICAL_WORK_PER_BYTE: usize = 2 * MAX_NESTING;

/// The work any file may take, however short, in each budget but the
/// parse's operations.
pub(super) const MIN_WORK: usize = 1 << 20;

/// What is left of one budget of a file.
#[derive(Debug)]
pub(super) struct Work {
    left: usize,
}

impl Work {
    /// The budget of `per_byte` units for each of `bytes`, and `min` at
    /// least.
    pub(super) fn of(bytes: usize, per_byte: usize, min: usize) -> Self {
        Self {
            left: bytes.saturating_mul(per_byte).max(min),
        }
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
    fn a_budget_is_linear_in_the_file_and_its_minimum_at_least() {
        assert!(holds_exactly(Work::of(100_000, 16, MIN_WORK), 1_600_000));
        assert!(holds_exactly(Work::of(10, 16, MIN_WORK), 1_048_576));
    }
}
