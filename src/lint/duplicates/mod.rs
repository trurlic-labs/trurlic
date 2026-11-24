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
//! The pairs come from an inverted index over the terms (all-pairs with a
//! prefix filter). Terms are ranked most common first, and the leading
//! terms of a decision whose norm is under the threshold's share of its
//! whole stay out of the index: a pair at the threshold shares a term both
//! index. A candidate's dot product over those shared terms bounds what it
//! can reach on the others (`vectors::may_reach`), and only one that can
//! reach the threshold is scored whole. No pair is visited that shares no
//! indexed term, and none allocates: the pass counts every pair and keeps
//! only the best it is asked for (`best`), however many there are.

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

/// The weight vectors of a graph's decisions.
pub(crate) struct Similarity<'s> {
    /// The decisions' names, sorted; a decision is its index here.
    names: Vec<&'s str>,
    vectors: Vec<Vector>,
    /// The terms of the graph: the ranks run below it.
    terms: usize,
}

impl<'s> Similarity<'s> {
    /// The vectors of every decision of `state`.
    pub(crate) fn of(state: &'s ProjectState) -> Self {
        let (vectors, terms) = vectors::of(state);
        Self {
            names: state.decisions.keys().map(String::as_str).collect(),
            vectors,
            terms,
        }
    }

    /// The count of likely duplicate pairs and the best `limit` of them:
    /// the pass holds no more than `limit` pairs per thread, however many
    /// there are.
    ///
    /// The decisions are put in the order of their boundaries, and each
    /// probes those before it, so the later boundary of every pair is the
    /// probe's and the probe's squares below it are its unindexed ones.
    /// The probes run on the thread pool, each task over a run of places in
    /// order, so the cursors it keeps into the earlier decisions' squares
    /// only move forward. The kept pairs are the smallest keys of a total
    /// order, so they do not depend on the threads.
    pub(crate) fn pairs(&self, limit: usize) -> Ranked<'s> {
        let mut order: Vec<usize> = (0..self.vectors.len()).collect();
        order.sort_unstable_by_key(|&decision| {
            (self.vectors.get(decision).map(|v| v.boundary), decision)
        });
        let index = Index::of(&self.vectors, order, self.terms);
        let decisions = self.vectors.len();
        let best = (0..decisions)
            .into_par_iter()
            .with_min_len(PER_TASK)
            .fold(
                || Task::new(decisions, limit),
                |mut task, place| {
                    self.probe(&index, place, &mut task);
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

    /// Offer to `best` each likely duplicate of the decision at `place` in
    /// `index` among those before it.
    fn probe(&self, index: &Index, place: usize, task: &mut Task) {
        let Task {
            scratch,
            cursors,
            best,
        } = task;
        let Some(probe) = index.member(place) else {
            return;
        };
        let Some(vector) = self.vectors.get(probe) else {
            return;
        };
        let read = Places {
            before: place,
            skip: usize::MAX,
        };
        scratch.candidates(index, vector, read, |other, partial| {
            let (Some(held), Some(cursor)) = (self.vectors.get(other), cursors.get_mut(other))
            else {
                return;
            };
            let rests = (
                vector.unindexed,
                held.square_below_from(cursor, vector.boundary),
            );
            if vectors::may_reach(vector, held, partial, rests)
                && let Some(score) = likely(vector, held)
            {
                best.offer((Reverse(score), probe.min(other), probe.max(other)));
            }
        });
    }

    /// The likely duplicates of each decision of `names`, in their order:
    /// best first, then by name, at most `count`; none for a name the graph
    /// lacks.
    pub(crate) fn nearest(&self, names: &[&str], count: usize) -> Vec<Vec<Neighbour<'s>>> {
        let decisions = self.vectors.len();
        // Every decision at the place of its own index.
        let index = Index::of(&self.vectors, (0..decisions).collect(), self.terms);
        let mut scratch = Scratch::new(decisions);
        names
            .iter()
            .map(|name| self.nearest_in(&index, &mut scratch, name, count))
            .collect()
    }

    /// The likely duplicates of `name` in `index`, which holds every
    /// decision at the place of its own index.
    fn nearest_in(
        &self,
        index: &Index,
        scratch: &mut Scratch,
        name: &str,
        count: usize,
    ) -> Vec<Neighbour<'s>> {
        let Ok(decision) = self.names.binary_search(&name) else {
            return Vec::new();
        };
        let Some(vector) = self.vectors.get(decision) else {
            return Vec::new();
        };
        let mut best: Best<(Reverse<Score>, usize)> = Best::new(count);
        let everyone = Places {
            before: self.vectors.len(),
            skip: decision,
        };
        scratch.candidates(index, vector, everyone, |other, partial| {
            let Some(held) = self.vectors.get(other) else {
                return;
            };
            let boundary = vector.boundary.max(held.boundary);
            let rests = (vector.square_below(boundary), held.square_below(boundary));
            if vectors::may_reach(vector, held, partial, rests)
                && let Some(score) = likely(vector, held)
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

    /// The decisions `first` and `second` as a pair, when they are likely
    /// duplicates: what the pass finds, decided one pair at a time.
    #[cfg(test)]
    fn pair(&self, first: usize, second: usize) -> Option<Pair<'s>> {
        let score = likely(self.vectors.get(first)?, self.vectors.get(second)?)?;
        Some(Pair {
            first: self.names.get(first)?,
            second: self.names.get(second)?,
            score,
        })
    }
}

/// The score of `a` and `b` when they share a term and their cosine is at
/// the threshold or above: `(denominator dot)^2 >= numerator^2 |a|^2
/// |b|^2`, exact in 256 bits.
fn likely(a: &Vector, b: &Vector) -> Option<Score> {
    let dot = a.dot(b);
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

/// An inverted index over the indexed terms of the decisions of `members`,
/// each by its place there.
struct Index {
    /// The decision at each place.
    members: Vec<usize>,
    /// Per term, by rank, the places of the decisions that index it, in
    /// order, with its weight there.
    postings: Vec<Vec<(usize, u64)>>,
}

impl Index {
    fn of(vectors: &[Vector], members: Vec<usize>, terms: usize) -> Self {
        let mut postings = vec![Vec::new(); terms];
        for (place, &decision) in members.iter().enumerate() {
            let indexed = vectors.get(decision).map_or(&[][..], Vector::indexed);
            for &(term, weight) in indexed {
                if let Some(posting) = postings.get_mut(term) {
                    posting.push((place, weight));
                }
            }
        }
        Self { members, postings }
    }

    fn member(&self, place: usize) -> Option<usize> {
        self.members.get(place).copied()
    }
}

/// What one task of the pairs pass holds over its run of probes.
struct Task {
    scratch: Scratch,
    /// Per decision, how many of its terms rank below the boundary of the
    /// task's last probe. A cursor that stood further on would make the
    /// bound of `may_reach` looser, never wrong.
    cursors: Vec<usize>,
    best: Best<PairKey>,
}

impl Task {
    fn new(decisions: usize, limit: usize) -> Self {
        Self {
            scratch: Scratch::new(decisions),
            cursors: vec![0; decisions],
            best: Best::new(limit),
        }
    }
}

/// The dot products of one probe: sized to the graph once and cleared
/// after each probe, so no pair allocates.
struct Scratch {
    /// Per place, its decision's dot product with the probe so far.
    partial: Vec<u128>,
    /// The places whose `partial` is above zero.
    touched: Vec<usize>,
}

/// The places a probe reads in an index: those before `before`, but the
/// one at `skip`.
#[derive(Debug, Clone, Copy)]
struct Places {
    before: usize,
    skip: usize,
}

impl Scratch {
    fn new(places: usize) -> Self {
        Self {
            partial: vec![0; places],
            touched: Vec::new(),
        }
    }

    /// Call `each` with every decision of `places` in `index` that shares
    /// a term with `probe` both index, and their dot product over the terms
    /// both index: every decision whose cosine with it can reach the
    /// threshold. Leaves the scratch clear.
    fn candidates(
        &mut self,
        index: &Index,
        probe: &Vector,
        places: Places,
        mut each: impl FnMut(usize, u128),
    ) {
        let Places { before, skip } = places;
        for &(term, weight) in probe.indexed() {
            let posting = index.postings.get(term).map_or(&[][..], Vec::as_slice);
            // A posting is in place order: the first at `before` ends it.
            for &(place, held) in posting.iter().take_while(|&&(place, _)| place < before) {
                let Some(partial) = self.partial.get_mut(place).filter(|_| place != skip) else {
                    continue;
                };
                if *partial == 0 {
                    self.touched.push(place);
                }
                // Weights are at least 2^32, so a product is above zero.
                *partial = partial.saturating_add(u128::from(weight) * u128::from(held));
            }
        }
        for place in self.touched.drain(..) {
            if let (Some(partial), Some(other)) = (self.partial.get_mut(place), index.member(place))
            {
                each(other, std::mem::take(partial));
            }
        }
    }
}

#[cfg(test)]
mod tests;
