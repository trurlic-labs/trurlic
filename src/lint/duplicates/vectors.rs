//! The decisions' TF-IDF vectors in integers: each term named by its id,
//! weighted by its count times its inverse frequency in Q32.

use std::collections::BTreeMap;
use std::ops::Range;

use super::{ONE_Q32, PER_TASK, fixed, terms};
use rayon::prelude::*;

use crate::store::{Decision, ProjectState};

/// A decision's terms and weights.
pub(super) struct Vector {
    /// (term, weight), each of the decision's terms once.
    pub(super) terms: Vec<(usize, u64)>,
    /// The sum of the squares of the weights.
    pub(super) square: u128,
    /// The integer square root of `square`.
    pub(super) norm: u128,
}

/// The vector of each decision of `state`, in name order, and the count of
/// terms the graph holds: the ids run below it.
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
    // Each term met so far and its id: the count of terms met before it.
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    let counted: Vec<Vec<(usize, u64)>> = found
        .iter()
        .map(|found| {
            let terms = found.terms();
            terms
                .map(|(term, count)| {
                    let next = ids.len();
                    (*ids.entry(term).or_insert(next), count)
                })
                .collect()
        })
        .collect();
    let inverse = inverse_frequencies(&counted, ids.len());
    let vectors = counted
        .into_iter()
        .map(|terms| vector(terms, &inverse))
        .collect();
    (vectors, ids.len())
}

/// The inverse frequency of each of `terms` terms, by id, over `counted`:
/// each decision's terms and their counts.
fn inverse_frequencies(counted: &[Vec<(usize, u64)>], terms: usize) -> Vec<u64> {
    let mut holding = vec![0u64; terms];
    for &(term, _) in counted.iter().flatten() {
        if let Some(count) = holding.get_mut(term) {
            *count = count.saturating_add(1);
        }
    }
    let decisions = u64::try_from(counted.len()).unwrap_or(u64::MAX);
    // Many terms share a count, so each count's logarithm is taken once.
    let mut by_count: Vec<Option<u64>> = vec![None; counted.len() + 1];
    holding
        .iter()
        .map(|&holding| {
            let once = usize::try_from(holding)
                .ok()
                .and_then(|at| by_count.get_mut(at));
            let fresh = || inverse_frequency(decisions, holding);
            once.map_or_else(fresh, |memo| *memo.get_or_insert_with(fresh))
        })
        .collect()
}

/// The vector of a decision whose terms and counts are `terms`, each term
/// weighed by its `inverse` frequency.
fn vector(mut terms: Vec<(usize, u64)>, inverse: &[u64]) -> Vector {
    let mut square = 0u128;
    for (term, weight) in &mut terms {
        let frequency = inverse.get(*term).copied().unwrap_or(ONE_Q32);
        *weight = weight.saturating_mul(frequency);
        square = square.saturating_add(u128::from(*weight).pow(2));
    }
    Vector {
        terms,
        square,
        norm: square.isqrt(),
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

impl Found {
    /// Each distinct term, sorted, with its count.
    fn terms(&self) -> impl Iterator<Item = (&str, u64)> {
        let text = |span: &Range<usize>| self.text.get(span.start..span.end).unwrap_or_default();
        self.terms
            .iter()
            .map(move |(span, count)| (text(span), *count))
    }
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
