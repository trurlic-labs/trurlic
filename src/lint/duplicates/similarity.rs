//! The weight vectors of a graph's decisions, the inverted index over
//! their terms ([`Similarity`]), and its two passes: the pairs of a graph
//! ([`Similarity::pairs`]) and the likely duplicates of given decisions
//! ([`Similarity::nearest`]).

use std::cmp::Reverse;
use std::collections::BTreeMap;

use rayon::prelude::*;

use super::best::Best;
use super::score::{Score, likely};
use super::vectors::{self, PER_TASK, Vector};
use crate::store::Decision;

/// The work charged for a pair a probe may test, in posting entries: its
/// threshold test, 256-bit products, the division of a score and its offer
/// to the selection cost about as much as this many entries read, measured
/// where half the pairs pass the threshold (terms each held by half the
/// decisions).
const PAIR_WORK: usize = 16;

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

/// The likely duplicate pairs one pass found, and how far its probes read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ranked<'s> {
    /// Every pair the probes found.
    pub(crate) total: usize,
    /// The best of them, best first, then by names.
    pub(crate) best: Vec<Pair<'s>>,
    /// The decisions probed: the first by name of those the pass looks for.
    pub(crate) compared: usize,
    /// The decisions the bound left unprobed, the last by name.
    pub(crate) uncompared: usize,
    /// The work of every probe the pass looks for, run or not.
    pub(crate) work: usize,
}

/// The pairs a pass looks for.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Among<'n> {
    /// Every pair of the graph.
    Every,
    /// The pairs that hold one of these decisions, by name, sorted; a name
    /// the graph lacks holds none.
    Touching(&'n [&'n str]),
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
    /// The vectors of every decision of `decisions`, and their index.
    pub(crate) fn of(decisions: &'s BTreeMap<String, Decision>) -> Self {
        let (vectors, terms) = vectors::of(decisions);
        let mut postings = vec![Vec::new(); terms];
        for (decision, vector) in vectors.iter().enumerate() {
            for &(term, weight) in &vector.terms {
                if let Some(posting) = postings.get_mut(term) {
                    posting.push((decision, weight));
                }
            }
        }
        Self {
            names: decisions.keys().map(String::as_str).collect(),
            vectors,
            postings,
        }
    }

    /// The likely duplicate pairs `among` that the probes find, run in
    /// name order while their work stays within `bound`: the count of them
    /// and the best `limit`. The pass holds no more than `limit` pairs per
    /// task of the thread pool, however many there are.
    ///
    /// Over every pair each decision probes those before it, so the pairs
    /// found are those among the decisions probed. Over the pairs touching
    /// some decisions only those are probed, each against every other but
    /// a probe before it, so the pairs found are those holding a decision
    /// probed. The probes run on the thread pool, and the kept pairs are
    /// the smallest keys of a total order, so they do not depend on the
    /// threads.
    pub(crate) fn pairs(&self, among: Among<'_>, limit: usize, bound: usize) -> Ranked<'s> {
        let probed = self.probed(among);
        let works = self.works(probed.as_deref());
        let probes: Vec<usize> = (0..works.len())
            .filter(|&decision| is_probed(probed.as_deref(), decision))
            .collect();
        let work = |probe: &usize| works.get(*probe).copied().unwrap_or(0);
        let mut spent = 0usize;
        let compared = probes
            .iter()
            .take_while(|probe| {
                spent = spent.saturating_add(work(probe));
                spent <= bound
            })
            .count();
        let run = probes.get(..compared).unwrap_or_default();
        let selection = self.selection(run, probed.as_deref(), limit);
        let (total, kept) = selection.into_sorted();
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
        Ranked {
            total,
            best,
            compared,
            uncompared: probes.len() - compared,
            work: probes.iter().map(work).fold(0, usize::saturating_add),
        }
    }

    /// Per decision, whether it is probed: `None` over every pair.
    fn probed(&self, among: Among<'_>) -> Option<Vec<bool>> {
        let Among::Touching(names) = among else {
            return None;
        };
        let mut probed = vec![false; self.vectors.len()];
        for name in names {
            if let Some(slot) = self
                .names
                .binary_search(name)
                .ok()
                .and_then(|decision| probed.get_mut(decision))
            {
                *slot = true;
            }
        }
        Some(probed)
    }

    /// Each decision's work as a probe, 0 for one not probed: the posting
    /// entries it reads, and [`PAIR_WORK`] for each pair it may test, at
    /// most one per entry read and one per decision it reaches.
    fn works(&self, probed: Option<&[bool]>) -> Vec<usize> {
        let decisions = self.vectors.len();
        let mut entries = vec![0usize; decisions];
        for posting in &self.postings {
            for (position, &(decision, _)) in posting.iter().enumerate() {
                if let Some(read) = entries.get_mut(decision) {
                    *read = read.saturating_add(match probed {
                        // Over every pair, the entries before its own.
                        None => position,
                        Some(_) => posting.len(),
                    });
                }
            }
        }
        let reach = |decision: usize| match probed {
            None => decision,
            Some(_) => decisions.saturating_sub(1),
        };
        let work = |(decision, read): (usize, usize)| match is_probed(probed, decision) {
            true => read.saturating_add(read.min(reach(decision)).saturating_mul(PAIR_WORK)),
            false => 0,
        };
        entries.into_iter().enumerate().map(work).collect()
    }

    /// The selection of the best `limit` pairs the decisions `probes` find,
    /// probed on the thread pool.
    fn selection(&self, probes: &[usize], probed: Option<&[bool]>, limit: usize) -> Best<PairKey> {
        let decisions = self.vectors.len();
        probes
            .par_iter()
            .with_min_len(PER_TASK)
            .fold(
                || Task::new(decisions, limit),
                |mut task, &probe| {
                    self.probe(probe, probed, &mut task);
                    task
                },
            )
            .map(|task| task.best)
            .reduce(|| Best::new(limit), Best::merge)
    }

    /// Offer to the task's selection each likely duplicate of the decision
    /// `probe`: among those before it over every pair, else among every
    /// other decision but a probed one before it, which offers that pair.
    fn probe(&self, probe: usize, probed: Option<&[bool]>, task: &mut Task) {
        let Task { scratch, best } = task;
        let Some(vector) = self.vectors.get(probe) else {
            return;
        };
        let before = match probed {
            Some(_) => self.vectors.len(),
            None => probe,
        };
        let offered_by = |other: usize| {
            other < probe && probed.is_some_and(|probed| probed.get(other) == Some(&true))
        };
        scratch.dots(&self.postings, vector, before, |other, dot| {
            if other == probe || offered_by(other) {
                return;
            }
            if let Some(score) = self
                .vectors
                .get(other)
                .and_then(|held| likely(vector, held, dot))
            {
                best.offer((Reverse(score), other.min(probe), other.max(probe)));
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

#[cfg(test)]
impl<'s> Similarity<'s> {
    /// The decisions' names, sorted: a decision is its index here.
    pub(super) fn names(&self) -> &[&'s str] {
        &self.names
    }

    /// The decisions' vectors, by index.
    pub(super) fn vectors(&self) -> &[Vector] {
        &self.vectors
    }
}

/// Whether `decision` is probed: every one when `probed` is `None`.
fn is_probed(probed: Option<&[bool]>, decision: usize) -> bool {
    probed.is_none_or(|probed| probed.get(decision) == Some(&true))
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
