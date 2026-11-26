//! A likely duplicate's score: the cosine of two decisions' vectors in
//! Q32 ([`Score`]), and the exact test that puts a pair at the threshold
//! or above ([`likely`]).

use std::fmt;

use super::fixed::{self, ONE_Q32};
use super::vectors::Vector;

/// A cosine in units of 2^-32, from 0 to 2^32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Score(u64);

impl Score {
    /// The score at the threshold, 1/2: the least a likely duplicate has.
    pub(crate) const THRESHOLD: Self = Self(ONE_Q32 / 2);
}

#[cfg(test)]
impl Score {
    /// The cosine in units of 2^-32.
    pub(super) const fn q32(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Score {
    /// Six decimals, rounded half up: `0.544842`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let micros = (u128::from(self.0) * 1_000_000 + (1 << 31)) >> 32;
        write!(f, "{}.{:06}", micros / 1_000_000, micros % 1_000_000)
    }
}

/// The score of `a` and `b`, whose dot product is `dot`, when they share a
/// term and their cosine is [`Score::THRESHOLD`], 1/2, or above: `(2
/// dot)^2 >= |a|^2 |b|^2`, exact in 256 bits.
pub(super) fn likely(a: &Vector, b: &Vector, dot: u128) -> Option<Score> {
    if dot == 0 {
        return None;
    }
    let doubled = dot.saturating_mul(2);
    if fixed::wide(doubled, doubled) < fixed::wide(a.square, b.square) {
        return None;
    }
    // A shared term puts a weight of at least 2^32 in each vector, so both
    // norms are at least 2^32, the divisor at least 2^32 and the quotient a
    // cosine in Q32.
    let divisor = (a.norm.saturating_mul(b.norm) >> 32).max(1);
    let cosine = (dot / divisor).min(u128::from(ONE_Q32));
    Some(Score(u64::try_from(cosine).unwrap_or(ONE_Q32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_score_prints_six_decimals_rounded_half_up() {
        let shown = |units: u64| Score(units).to_string();
        assert_eq!(shown(0), "0.000000");
        assert_eq!(shown(1 << 31), "0.500000");
        assert_eq!(shown(1 << 32), "1.000000");
        // 0.0000005 is 2147.48 units: one above it rounds up, one below down.
        assert_eq!(shown(2148), "0.000001");
        assert_eq!(shown(2147), "0.000000");
    }
}
