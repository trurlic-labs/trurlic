//! The budget of work an index may take after its file is parsed: each
//! token or byte a span hash covers, and each token a lexical rule scans,
//! is spent from it, and an index past it is refused as
//! [`IndexFault::TooComplex`]. A rule that would run quadratic on some
//! input is stopped at a bound linear in the file, on every machine
//! alike, rather than found by its time.

use super::index::IndexFault;
use super::scope::MAX_NESTING;

/// Units of work per byte of the file. A definition's hash covers the
/// tokens of every definition inside it, so a token is hashed once per
/// container around it, at most [`MAX_NESTING`] times; twice that leaves
/// the scans of the lexical rules their own share.
pub(super) const MAX_WORK_PER_BYTE: usize = 2 * MAX_NESTING;

/// The work any file may take, however short.
const MIN_WORK: usize = 1 << 20;

/// What is left of a file's budget.
#[derive(Debug)]
pub(super) struct Work {
    left: usize,
}

impl Work {
    /// The budget of a file of `bytes` bytes.
    pub(super) fn of(bytes: usize) -> Self {
        Self {
            left: bytes.saturating_mul(MAX_WORK_PER_BYTE).max(MIN_WORK),
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

    #[test]
    fn a_budget_is_linear_in_the_file_and_refuses_past_it() {
        let bytes = MIN_WORK;
        let mut work = Work::of(bytes);

        assert_eq!(work.spend(bytes * MAX_WORK_PER_BYTE), Ok(()));
        assert_eq!(work.spend(1), Err(IndexFault::TooComplex));
        assert!(Work::of(0).spend(MIN_WORK).is_ok());
    }
}
