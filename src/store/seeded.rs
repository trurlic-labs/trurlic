//! The one seeded source of the corpus and of the crate's property tests:
//! SplitMix64 rather than `rand`, whose generators may change output
//! between releases, so a seed draws the same values in every build.

use std::ops::RangeInclusive;

/// SplitMix64 (Steele, Lea and Flood, 2014): a 64-bit state, full period,
/// and an output fixed by its definition.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// In `0..n` by multiply-shift: no division, and each value's share off
    /// from `1/n` by under `2^-64`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        let bound = u128::try_from(n).unwrap_or(u128::MAX);
        let draw = (u128::from(self.next()) * bound) >> 64;
        // `draw < n`, so the conversion holds.
        usize::try_from(draw).unwrap_or(0)
    }

    pub(crate) fn within(&mut self, range: &RangeInclusive<usize>) -> usize {
        range.start() + self.below(range.end() - range.start() + 1)
    }

    pub(super) fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
}
