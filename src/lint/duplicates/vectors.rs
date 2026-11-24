//! The decisions' TF-IDF vectors in integers: each term named by its rank,
//! the most common term first, weighted by its count times its inverse
//! frequency in Q32; and the part of a vector the index leaves out.

use std::cmp::Reverse;
use std::ops::Range;

use super::{ONE_Q32, PER_TASK, THRESHOLD, fixed, terms};
use rayon::prelude::*;

use crate::store::{Decision, ProjectState};

/// A decision's terms and weights.
pub(super) struct Vector {
    /// (term, weight), by the term's rank: the most common term first.
    terms: Vec<(usize, u64)>,
    /// At `k`, the sum of the squares of the first `k` weights.
    squares: Vec<u128>,
    /// The sum of the squares of the weights.
    pub(super) square: u128,
    /// The integer square root of `square`.
    pub(super) norm: u128,
    /// The terms before it are left out of the index: their norm is under
    /// the threshold's share of the whole, so no cosine reaches the
    /// threshold on them alone.
    indexed_from: usize,
    /// The rank of the first indexed term: every term ranked at it or after
    /// is indexed.
    pub(super) boundary: usize,
    /// The sum of the squares of the weights left out of the index.
    pub(super) unindexed: u128,
}

impl Vector {
    /// The terms the index holds: every term from the first indexed one's
    /// rank on.
    pub(super) fn indexed(&self) -> &[(usize, u64)] {
        self.terms.get(self.indexed_from..).unwrap_or_default()
    }

    /// The exact dot product with `other`.
    pub(super) fn dot(&self, other: &Self) -> u128 {
        let (mut mine, mut theirs) = (self.terms.iter().peekable(), other.terms.iter().peekable());
        let mut dot = 0u128;
        while let (Some(&&(a, weight)), Some(&&(b, held))) = (mine.peek(), theirs.peek()) {
            if a <= b {
                mine.next();
            }
            if b <= a {
                theirs.next();
            }
            if a == b {
                dot = dot.saturating_add(u128::from(weight) * u128::from(held));
            }
        }
        dot
    }

    #[cfg(test)]
    pub(super) fn terms(&self) -> &[(usize, u64)] {
        &self.terms
    }

    /// The sum of the squares of the weights of the terms ranked below
    /// `rank`.
    pub(super) fn square_below(&self, rank: usize) -> u128 {
        let count = if rank == self.boundary {
            self.indexed_from
        } else {
            self.terms.partition_point(|&(term, _)| term < rank)
        };
        self.squares.get(count).copied().unwrap_or(0)
    }

    /// [`square_below`](Self::square_below) a `rank` at or after the
    /// boundary and the rank of the call before, found from where `cursor`
    /// was left: over a pass whose ranks only grow, the cursor walks the
    /// terms once.
    pub(super) fn square_below_from(&self, cursor: &mut usize, rank: usize) -> u128 {
        let mut count = (*cursor).max(self.indexed_from);
        while self.terms.get(count).is_some_and(|&(term, _)| term < rank) {
            count += 1;
        }
        *cursor = count;
        self.squares.get(count).copied().unwrap_or(0)
    }
}

/// Whether the cosine of `a` and `b`, whose dot product over the terms
/// both index is `partial`, can reach the threshold, when `rests` are the
/// sums of the squares of their weights below the later of their
/// boundaries. The terms both index are those ranked at that boundary or
/// after; on the others the dot product is at most the product of the two
/// vectors' norms there (Cauchy-Schwarz). So it cannot when `denominator
/// (partial + sqrt(rest_a rest_b)) < numerator |a| |b|`, which the integer
/// square roots of the norms, rounded down, can only make harder to show.
pub(super) fn may_reach(a: &Vector, b: &Vector, partial: u128, rests: (u128, u128)) -> bool {
    let (rest_a, rest_b) = rests;
    let least = THRESHOLD
        .numerator
        .saturating_mul(a.norm.saturating_mul(b.norm));
    let Some(room) = least.checked_sub(THRESHOLD.denominator.saturating_mul(partial)) else {
        return true;
    };
    let rest = THRESHOLD.denominator.pow(2).saturating_mul(rest_a);
    fixed::wide(rest, rest_b) >= fixed::wide(room, room)
}

/// The vector of each decision of `state`, by name, and the count of
/// terms the graph holds, which name the ranks.
pub(super) fn of(state: &ProjectState) -> (Vec<Vector>, usize) {
    // Each decision's terms are found and counted on the thread pool; the
    // ids are given after, in one pass in the order of the decisions, so no
    // id depends on the threads.
    let decisions: Vec<&Decision> = state.decisions.values().collect();
    let found: Vec<Found> = decisions
        .par_iter()
        .with_min_len(PER_TASK)
        .map(|decision| found(decision))
        .collect();
    let mut vocabulary = Vocabulary::default();
    let counted: Vec<Vec<(usize, u64)>> = found
        .iter()
        .map(|found| {
            let terms = found.terms.iter();
            terms
                .map(|(span, count)| {
                    let term = found.text.get(span.start..span.end).unwrap_or_default();
                    (vocabulary.id(term), *count)
                })
                .collect()
        })
        .collect();
    let terms = vocabulary.0.len();
    let weights = Weights::of(&counted, terms);
    let vectors = counted
        .into_iter()
        .map(|terms| weights.vector(terms))
        .collect();
    (vectors, terms)
}

/// Each term met so far and its id, in the order the terms were first met:
/// decision by decision, each decision's terms in sorted order.
#[derive(Default)]
#[expect(
    clippy::disallowed_types,
    reason = "a lookup from a term to its id, never iterated: ids follow the order the terms \
              are first met in, so no answer depends on the map's order, and a BTreeMap of \
              strings took most of the pass on the seeded corpus"
)]
struct Vocabulary(std::collections::HashMap<String, usize>);

impl Vocabulary {
    fn id(&mut self, term: &str) -> usize {
        if let Some(&id) = self.0.get(term) {
            return id;
        }
        let id = self.0.len();
        self.0.insert(term.to_owned(), id);
        id
    }
}

/// The inverse frequency of each term, and its rank, most common first.
struct Weights {
    inverse: Vec<u64>,
    rank: Vec<usize>,
}

impl Weights {
    /// Over `counted`, each decision's terms and their counts, of
    /// `vocabulary` terms in all.
    fn of(counted: &[Vec<(usize, u64)>], vocabulary: usize) -> Self {
        let mut holding = vec![0u64; vocabulary];
        for &(term, _) in counted.iter().flatten() {
            if let Some(count) = holding.get_mut(term) {
                *count = count.saturating_add(1);
            }
        }
        let decisions = u64::try_from(counted.len()).unwrap_or(u64::MAX);
        // Many terms share a count, so each count's logarithm is taken once.
        let mut by_count: Vec<Option<u64>> = vec![None; counted.len() + 1];
        let inverse = holding
            .iter()
            .map(|&holding| {
                let once = usize::try_from(holding)
                    .ok()
                    .and_then(|at| by_count.get_mut(at));
                let fresh = || inverse_frequency(decisions, holding);
                once.map_or_else(fresh, |memo| *memo.get_or_insert_with(fresh))
            })
            .collect();
        let mut order: Vec<usize> = (0..vocabulary).collect();
        order.sort_unstable_by_key(|&term| (Reverse(holding.get(term).copied()), term));
        let mut rank = vec![0; vocabulary];
        for (place, term) in order.into_iter().enumerate() {
            if let Some(slot) = rank.get_mut(term) {
                *slot = place;
            }
        }
        Self { inverse, rank }
    }

    /// The vector of a decision whose terms and counts are `terms`.
    fn vector(&self, mut terms: Vec<(usize, u64)>) -> Vector {
        for (term, weight) in &mut terms {
            let inverse = self.inverse.get(*term).copied().unwrap_or(ONE_Q32);
            *weight = weight.saturating_mul(inverse);
            *term = self.rank.get(*term).copied().unwrap_or(usize::MAX);
        }
        terms.sort_unstable();
        let mut squares = Vec::with_capacity(terms.len() + 1);
        squares.push(0u128);
        for &(_, weight) in &terms {
            let last = squares.last().copied().unwrap_or(0);
            squares.push(last.saturating_add(u128::from(weight).pow(2)));
        }
        let square = squares.last().copied().unwrap_or(0);
        // The longest prefix under the threshold's share; `squares` grows.
        let below = |part: &u128| {
            let (numerator, denominator) = (THRESHOLD.numerator, THRESHOLD.denominator);
            fixed::wide(denominator.pow(2), *part) < fixed::wide(numerator.pow(2), square)
        };
        let indexed_from = squares.get(1..).unwrap_or_default().partition_point(below);
        let boundary = terms
            .get(indexed_from)
            .map_or(usize::MAX, |&(term, _)| term);
        let unindexed = squares.get(indexed_from).copied().unwrap_or(0);
        Vector {
            terms,
            squares,
            square,
            norm: square.isqrt(),
            indexed_from,
            boundary,
            unindexed,
        }
    }
}

/// The inverse frequency, in Q32, of a term `holding` of `decisions`
/// decisions hold: ln((1 + decisions) / (1 + holding)) + 1, rounded half
/// up, within 2^-32 of the exact value.
pub(super) fn inverse_frequency(decisions: u64, holding: u64) -> u64 {
    let ln = |count: u64| fixed::ln(count.saturating_add(1));
    let ratio = ln(decisions).saturating_sub(ln(holding));
    // ratio < 45 * 2^64, so the Q32 value stays below 2^38.
    let q32 = (ratio + (1 << 31)) >> 32;
    u64::try_from(q32)
        .unwrap_or(u64::MAX)
        .saturating_add(ONE_Q32)
}

/// A decision's distinct terms, sorted, each with its count: the terms are
/// spans of `text`, which holds every occurrence one after another.
struct Found {
    text: String,
    terms: Vec<(Range<usize>, u64)>,
}

fn found(decision: &Decision) -> Found {
    let mut text = String::new();
    let mut spans = Vec::new();
    terms::each_term(&decision.choice, &decision.reason, |term| {
        let start = text.len();
        text.push_str(term);
        spans.push(start..text.len());
    });
    let term = |span: &Range<usize>| text.get(span.start..span.end).unwrap_or_default();
    spans.sort_unstable_by(|a, b| term(a).cmp(term(b)));
    let mut terms: Vec<(Range<usize>, u64)> = Vec::new();
    for span in spans {
        match terms.last_mut() {
            Some((last, count)) if term(last) == term(&span) => *count += 1,
            Some(_) | None => terms.push((span, 1)),
        }
    }
    Found { text, terms }
}
