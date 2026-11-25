//! Likely duplicates: two decisions whose `choice` and `reason` say much
//! the same, by the cosine of their TF-IDF vectors.
//!
//! A decision's vector weighs each of its terms (`terms`) by its count
//! times the smoothed inverse document frequency ln((1 + n) / (1 + df)) +
//! 1, over the n decisions of the graph, df of which hold the term. A pair
//! at a cosine of at least [`THRESHOLD`] is a likely duplicate: a pair to
//! read, which no rule refuses.
//!
//! Integer throughout, so a score is a function of the files alone: the
//! inverse frequency in Q32 from the fixed-point logarithm (`fixed`),
//! within 2^-32 of the exact value; the weights, their squares and the dot
//! products exact; the threshold test exact over those weights, as
//! `(2 dot)^2 >= |a|^2 |b|^2` in 256 bits, so a pair whose cosine over them
//! is exactly 1/2 is listed. A weight off by a relative 2^-32 moves a
//! cosine by at most 2^-30, so the pairs are the real-valued computation's
//! but for one within that of the threshold. A [`Score`] divides by integer
//! square roots, off by at most 2^-30 more: within 2^-29 of the real-valued
//! cosine.
//!
//! The pairs come from an inverted index over every term: each decision
//! reads the postings of its terms for the decisions before it, adding up
//! its exact dot product with each, and every decision it shares a term
//! with is put to the threshold test. No pair is visited that shares no
//! term, and none allocates: the pass counts every pair and keeps only the
//! best it is asked for (`best`), however many there are.

mod best;
mod fixed;
mod terms;
mod vectors;

use std::cmp::Reverse;
use std::fmt;

use rayon::prelude::*;

use crate::store::ProjectState;
use best::Best;
use vectors::Vector;

/// The cosine at or above which two decisions are likely duplicates: 1/2.
const THRESHOLD: Ratio = Ratio {
    numerator: 1,
    denominator: 2,
};

/// A cosine bound as a ratio of integers, so the threshold test is exact.
struct Ratio {
    numerator: u128,
    denominator: u128,
}

/// One in Q32, the fixed point of the inverse frequencies and of a
/// [`Score`].
const ONE_Q32: u64 = 1 << 32;

/// The fewest decisions one task of the thread pool takes, so a small
/// graph runs as one task rather than paying for threads it cannot use.
const PER_TASK: usize = 32;

/// A cosine in units of 2^-32, from 0 to 2^32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Score(u64);

impl Score {
    /// The score at the threshold, the least a likely duplicate has.
    pub(crate) const THRESHOLD: Self = Self(ONE_Q32 / 2);
}

impl fmt::Display for Score {
    /// Six decimals, rounded half up: `0.544842`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let micros = (u128::from(self.0) * 1_000_000 + (1 << 31)) >> 32;
        write!(f, "{}.{:06}", micros / 1_000_000, micros % 1_000_000)
    }
}

/// Two likely duplicates, `first` before `second` by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pair<'s> {
    pub(crate) first: &'s str,
    pub(crate) second: &'s str,
    pub(crate) score: Score,
}

/// A likely duplicate of a given decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Neighbour<'s> {
    pub(crate) name: &'s str,
    pub(crate) score: Score,
}

/// The likely duplicate pairs of a graph: how many there are, and the best
/// of them, best first, then by names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ranked<'s> {
    pub(crate) total: usize,
    pub(crate) best: Vec<Pair<'s>>,
}

/// A pair as the selection orders it: the higher score first, then the
/// first decision, then the second, by index, which is name order.
type PairKey = (Reverse<Score>, usize, usize);

/// The weight vectors of a graph's decisions, and the inverted index over
/// their terms.
pub(crate) struct Similarity<'s> {
    /// The decisions' names, sorted; a decision is its index here.
    names: Vec<&'s str>,
    vectors: Vec<Vector>,
    /// Per term, by id, the decisions that hold it, in order, with its
    /// weight there.
    postings: Vec<Vec<(usize, u64)>>,
}

impl<'s> Similarity<'s> {
    /// The vectors of every decision of `state`, and their index.
    pub(crate) fn of(state: &'s ProjectState) -> Self {
        let (vectors, terms) = vectors::of(state);
        let mut postings = vec![Vec::new(); terms];
        for (decision, vector) in vectors.iter().enumerate() {
            for &(term, weight) in &vector.terms {
                if let Some(posting) = postings.get_mut(term) {
                    posting.push((decision, weight));
                }
            }
        }
        Self {
            names: state.decisions.keys().map(String::as_str).collect(),
            vectors,
            postings,
        }
    }

    /// The count of likely duplicate pairs that touch a decision `touching`
    /// admits, by name, and the best `limit` of them: the pass holds no
    /// more than `limit` pairs per task of the thread pool, however many
    /// there are.
    ///
    /// Each decision probes those before it. The probes run on the thread
    /// pool, and the kept pairs are the smallest keys of a total order, so
    /// they do not depend on the threads.
    pub(crate) fn pairs(&self, limit: usize, touching: impl Fn(&str) -> bool + Sync) -> Ranked<'s> {
        let decisions = self.vectors.len();
        let best = (0..decisions)
            .into_par_iter()
            .with_min_len(PER_TASK)
            .fold(
                || Task::new(decisions, limit),
                |mut task, probe| {
                    self.probe(probe, &mut task, &touching);
                    task
                },
            )
            .map(|task| task.best)
            .reduce(|| Best::new(limit), Best::merge);
        let (total, kept) = best.into_sorted();
        let best = kept
            .into_iter()
            .filter_map(|(Reverse(score), first, second)| {
                Some(Pair {
                    first: self.names.get(first)?,
                    second: self.names.get(second)?,
                    score,
                })
            })
            .collect();
        Ranked { total, best }
    }

    /// Offer to the task's selection each likely duplicate of the decision
    /// `probe` among those before it.
    fn probe(&self, probe: usize, task: &mut Task, touching: &(impl Fn(&str) -> bool + Sync)) {
        let Task { scratch, best } = task;
        let Some(vector) = self.vectors.get(probe) else {
            return;
        };
        scratch.dots(&self.postings, vector, probe, |other, dot| {
            let Some(score) = self
                .vectors
                .get(other)
                .and_then(|held| likely(vector, held, dot))
            else {
                return;
            };
            let named =
                |decision: usize| self.names.get(decision).is_some_and(|&name| touching(name));
            if named(other) || named(probe) {
                best.offer((Reverse(score), other, probe));
            }
        });
    }

    /// The likely duplicates of each decision of `names`, in their order:
    /// best first, then by name, at most `count`; none for a name the graph
    /// lacks.
    pub(crate) fn nearest(&self, names: &[&str], count: usize) -> Vec<Vec<Neighbour<'s>>> {
        let mut scratch = Scratch::new(self.vectors.len());
        names
            .iter()
            .map(|name| self.nearest_to(&mut scratch, name, count))
            .collect()
    }

    /// The likely duplicates of `name`, best first, at most `count`.
    fn nearest_to(&self, scratch: &mut Scratch, name: &str, count: usize) -> Vec<Neighbour<'s>> {
        let Ok(decision) = self.names.binary_search(&name) else {
            return Vec::new();
        };
        let Some(vector) = self.vectors.get(decision) else {
            return Vec::new();
        };
        let mut best: Best<(Reverse<Score>, usize)> = Best::new(count);
        scratch.dots(&self.postings, vector, self.vectors.len(), |other, dot| {
            if other == decision {
                return;
            }
            if let Some(score) = self
                .vectors
                .get(other)
                .and_then(|held| likely(vector, held, dot))
            {
                best.offer((Reverse(score), other));
            }
        });
        let (_, kept) = best.into_sorted();
        kept.into_iter()
            .filter_map(|(Reverse(score), other)| {
                let name = self.names.get(other)?;
                Some(Neighbour { name, score })
            })
            .collect()
    }
}

/// The score of `a` and `b`, whose dot product is `dot`, when they share a
/// term and their cosine is at the threshold or above: `(denominator
/// dot)^2 >= numerator^2 |a|^2 |b|^2`, exact in 256 bits.
fn likely(a: &Vector, b: &Vector, dot: u128) -> Option<Score> {
    if dot == 0 {
        return None;
    }
    let scaled = THRESHOLD.denominator.saturating_mul(dot);
    let bound = THRESHOLD.numerator.pow(2).saturating_mul(a.square);
    if fixed::wide(scaled, scaled) < fixed::wide(bound, b.square) {
        return None;
    }
    // A shared term puts a weight of at least 2^32 in each vector, so both
    // norms are at least 2^32, the divisor at least 2^32 and the quotient a
    // cosine in Q32.
    let divisor = (a.norm.saturating_mul(b.norm) >> 32).max(1);
    let cosine = (dot / divisor).min(u128::from(ONE_Q32));
    Some(Score(u64::try_from(cosine).unwrap_or(ONE_Q32)))
}

/// What one task of the pairs pass holds over its run of probes.
struct Task {
    scratch: Scratch,
    best: Best<PairKey>,
}

impl Task {
    fn new(decisions: usize, limit: usize) -> Self {
        Self {
            scratch: Scratch::new(decisions),
            best: Best::new(limit),
        }
    }
}

/// The dot products of one probe: sized to the graph once and cleared
/// after each probe, so no pair allocates.
struct Scratch {
    /// Per decision, its dot product with the probe so far.
    partial: Vec<u128>,
    /// The decisions whose `partial` is above zero.
    touched: Vec<usize>,
}

impl Scratch {
    fn new(decisions: usize) -> Self {
        Self {
            partial: vec![0; decisions],
            touched: Vec::new(),
        }
    }

    /// Call `each` with every decision before `before` in `postings` that
    /// shares a term with `probe`, and their dot product. Leaves the
    /// scratch clear.
    fn dots(
        &mut self,
        postings: &[Vec<(usize, u64)>],
        probe: &Vector,
        before: usize,
        mut each: impl FnMut(usize, u128),
    ) {
        for &(term, weight) in &probe.terms {
            let posting = postings.get(term).map_or(&[][..], Vec::as_slice);
            // A posting is in decision order: the first at `before` ends it.
            for &(other, held) in posting.iter().take_while(|&&(other, _)| other < before) {
                let Some(partial) = self.partial.get_mut(other) else {
                    continue;
                };
                if *partial == 0 {
                    self.touched.push(other);
                }
                // Weights are at least 2^32, so a product is above zero.
                *partial = partial.saturating_add(u128::from(weight) * u128::from(held));
            }
        }
        for other in self.touched.drain(..) {
            if let Some(partial) = self.partial.get_mut(other) {
                each(other, std::mem::take(partial));
            }
        }
    }
}

#[cfg(test)]
mod tests;
