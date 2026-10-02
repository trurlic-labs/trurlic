//! Fitting agent-facing output into a byte budget.
//!
//! An output is a fixed part plus [`Shrinkable`] parts: lists that drop
//! trailing entries, texts that drop trailing characters. [`level`] finds the
//! highest level every part may fill: a part no larger than the level stays
//! whole, a larger one is cut to it. A short list therefore survives intact
//! while the longest ones give up their tails, and a cut part also pays for
//! the note that reports what it lost.

/// Bytes of one MCP tool result's text. Claude Code warns above 10,000
/// tokens of tool output and moves a result above 25,000 tokens to a file;
/// 24 KiB of JSON or prose stays under the warning.
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// A part of an output that can shrink.
pub(crate) trait Shrinkable {
    /// Bytes of the whole part.
    fn full(&self) -> usize;

    /// Upper bound on the bytes of the note that reports a cut of this part.
    fn note(&self) -> usize;

    /// Bytes of the longest cut of this part within `limit`, at most `limit`.
    fn kept(&self, limit: usize) -> usize;
}

/// The limit to cut `part` to at `level`, or `None` when it stays whole:
/// it fits the level, or no cut is smaller than the part itself.
pub(crate) fn limit(part: &impl Shrinkable, level: usize) -> Option<usize> {
    let full = part.full();
    if full <= level {
        return None;
    }
    let limit = level.saturating_sub(part.note());
    (part.kept(limit).saturating_add(part.note()) < full).then_some(limit)
}

/// Bytes `part` takes at `level`, the note of a cut included. Never
/// decreases as `level` grows, which is what lets [`level`] bisect.
fn bytes_at(part: &impl Shrinkable, level: usize) -> usize {
    match limit(part, level) {
        Some(limit) => part.kept(limit) + part.note(),
        None => part.full(),
    }
}

/// The highest level at which `fixed` plus every part fits in `budget`, or
/// `None` when they exceed it even with every part cut to nothing. At the
/// largest part's size nothing is cut.
pub(crate) fn level<P: Shrinkable>(parts: &[P], fixed: usize, budget: usize) -> Option<usize> {
    let fits = |level: usize| {
        parts
            .iter()
            .try_fold(fixed, |sum, part| sum.checked_add(bytes_at(part, level)))
            .is_some_and(|total| total <= budget)
    };
    let mut high = parts.iter().map(Shrinkable::full).max().unwrap_or(0);
    if fits(high) {
        return Some(high);
    }
    if !fits(0) {
        return None;
    }
    // fits(low) holds and fits(high) does not.
    let mut low = 0;
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if fits(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    Some(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list of entries of the given sizes, with a note of `note` bytes.
    struct Entries {
        sizes: Vec<usize>,
        note: usize,
    }

    impl Shrinkable for Entries {
        fn full(&self) -> usize {
            self.sizes.iter().sum()
        }

        fn note(&self) -> usize {
            self.note
        }

        fn kept(&self, limit: usize) -> usize {
            let mut kept = 0;
            for size in &self.sizes {
                if kept + size > limit {
                    break;
                }
                kept += size;
            }
            kept
        }
    }

    fn entries(sizes: &[usize]) -> Entries {
        Entries {
            sizes: sizes.to_vec(),
            note: 5,
        }
    }

    fn total(parts: &[Entries], fixed: usize, level: usize) -> usize {
        fixed
            + parts
                .iter()
                .map(|part| bytes_at(part, level))
                .sum::<usize>()
    }

    #[test]
    fn a_short_part_stays_whole_while_the_long_one_is_cut() {
        let parts = [entries(&[10, 10]), entries(&[10; 20])];
        let level = level(&parts, 0, 100).unwrap();

        assert_eq!(limit(&parts[0], level), None);
        let cut = limit(&parts[1], level).unwrap();
        assert_eq!(parts[1].kept(cut), 70, "level {level}");
        assert!(total(&parts, 0, level) <= 100);
    }

    #[test]
    fn the_level_is_the_highest_that_fits() {
        let parts = [entries(&[7, 3, 9, 1]), entries(&[4; 9]), entries(&[30])];
        for budget in 0..=90 {
            let Some(level) = level(&parts, 6, budget) else {
                assert!(total(&parts, 6, 0) > budget, "budget {budget}");
                continue;
            };
            assert!(total(&parts, 6, level) <= budget, "budget {budget}");
            let largest = parts.iter().map(Shrinkable::full).max().unwrap();
            if level < largest {
                assert!(total(&parts, 6, level + 1) > budget, "budget {budget}");
            }
        }
    }

    #[test]
    fn bytes_never_decrease_as_the_level_grows() {
        // The note is larger than the part, so a cut never pays.
        let parts = [entries(&[2, 2]), entries(&[40, 1, 25]), entries(&[3; 30])];
        for part in &parts {
            let mut previous = 0;
            for level in 0..=120 {
                let bytes = bytes_at(part, level);
                assert!(bytes >= previous, "level {level}");
                previous = bytes;
            }
        }
    }

    #[test]
    fn a_part_smaller_than_its_note_is_never_cut() {
        let tiny = entries(&[1, 1]);
        for level in 0..10 {
            assert_eq!(limit(&tiny, level), None);
        }
    }

    #[test]
    fn nothing_fits_when_the_fixed_part_exceeds_the_budget() {
        assert_eq!(level(&[entries(&[10])], 50, 40), None);
    }
}
