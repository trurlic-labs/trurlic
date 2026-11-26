//! Likely duplicates: two decisions whose `choice` and `reason` say much
//! the same, by the cosine of their TF-IDF vectors.
//!
//! A decision's vector weighs each of its terms (`terms`) by its count
//! times the smoothed inverse document frequency ln((1 + n) / (1 + df)) +
//! 1, over the n decisions of the graph, df of which hold the term. A pair
//! at a cosine of at least [`Score::THRESHOLD`], 1/2, is a likely
//! duplicate: a pair to read, which no rule refuses.
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
//! probed reads the postings of its terms, adding up its exact dot product
//! with each decision it meets there, and every decision it shares a term
//! with is put to the threshold test. No pair is visited that shares no
//! term, and none allocates. Every step's work is counted before it runs
//! and charged against a bound the caller gives: the vectors' build by the
//! bytes it reads ([`Similarity::of`]), a probe by the posting entries it
//! reads and the pairs it may test ([`Similarity::page`]).

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

/// One in Q32, the fixed point of the inverse frequencies and of a
/// [`Score`].
const ONE_Q32: u64 = 1 << 32;

/// The work charged for a byte of `choice` and `reason` the vectors' build
/// reads, a decision counted as [`DECISION_BYTES`] more, in posting
/// entries: splitting it into terms, the vocabulary and the postings cost
/// about as much as this many entries read, at most, on the shapes
/// measured (short texts, long ones, the seeded corpus).
const BYTE_WORK: usize = 16;

/// The bytes a decision's own work in the build is counted as, beside its
/// text: its term list, its vector and its place in the vocabulary.
const DECISION_BYTES: usize = 64;

/// The work charged for a pair a probe may test, in posting entries: its
/// threshold test, 256-bit products, the division of a score and its offer
/// to the selection cost about as much as this many entries read, measured
/// where half the pairs pass the threshold (terms each held by half the
/// decisions).
const PAIR_WORK: usize = 16;

/// The fewest decisions one task of the thread pool takes, so a small
/// graph runs as one task rather than paying for threads it cannot use.
const PER_TASK: usize = 32;

/// A cosine in units of 2^-32, from 0 to 2^32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Score(u64);

impl Score {
    /// The score at the threshold, 1/2: the least a likely duplicate has.
    pub(crate) const THRESHOLD: Self = Self(ONE_Q32 / 2);
}

impl fmt::Display for Score {
    /// Six decimals, rounded half up: `0.544842`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let micros = (u128::from(self.0) * 1_000_000 + (1 << 31)) >> 32;
        write!(f, "{}.{:06}", micros / 1_000_000, micros % 1_000_000)
    }
}

/// Two likely duplicates, `first` before `second` by name, on the page of
/// `probe`, the one of them the pass probed for the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pair<'s> {
    pub(crate) first: &'s str,
    pub(crate) second: &'s str,
    pub(crate) score: Score,
    pub(crate) probe: &'s str,
}

/// A likely duplicate of a given decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Neighbour<'s> {
    pub(crate) name: &'s str,
    pub(crate) score: Score,
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

/// A step that did not run: its work, past the bound it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PastBound {
    pub(crate) work: usize,
}

/// One page of the pass: the probes from the first at or after `from` by
/// name, while their work fits, and the pairs they found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Page<'s> {
    /// The pairs of the page's probes, best first, then by names: every
    /// one but past the best `limit` of a first probe that alone fills it.
    pub(crate) pairs: Vec<Pair<'s>>,
    /// The count of the pairs of the page's probes.
    pub(crate) total: usize,
    /// The probes it ran before `next`, by name.
    pub(crate) run: Vec<&'s str>,
    /// The count of the probes from its first on.
    pub(crate) probes: usize,
    /// The first probe it left out and why, when it left one out.
    pub(crate) next: Option<(&'s str, PageEnd)>,
    /// The work of the build and of every probe from the page's first on.
    pub(crate) work: usize,
}

/// Why a page ended before its last probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageEnd {
    /// The probe would take the work past the bound.
    Work,
    /// The pairs of the probes before it fill the page.
    Pairs,
}

/// A pair as the selection orders it: its probe, the higher score first,
/// then its other decision, by index, which is name order.
type PairKey = (usize, Reverse<Score>, usize);

/// The weight vectors of a graph's decisions, the inverted index over
/// their terms, and the work left of the bound they were built within.
pub(crate) struct Similarity<'s> {
    /// The decisions' names, sorted; a decision is its index here.
    names: Vec<&'s str>,
    vectors: Vec<Vector>,
    /// Per term, by id, the decisions that hold it, in order, with its
    /// weight there.
    postings: Vec<Vec<(usize, u64)>>,
    /// The work the build charged.
    built: usize,
    bound: usize,
}

/// How a pass probes: each decision against those before it, or the
/// decisions an origin admits, each against every other but an admitted
/// one after it.
#[derive(Debug, Clone, Copy)]
enum Probing<'a> {
    Earlier,
    Admitted(&'a [bool]),
}

impl<'s> Similarity<'s> {
    /// The vectors of every decision of `state` and their index, when the
    /// build's work, [`BYTE_WORK`] per byte of the decisions' `choice` and
    /// `reason` and [`DECISION_BYTES`] more each, is within `bound`; else
    /// that work, and nothing built.
    pub(crate) fn of(state: &'s ProjectState, bound: usize) -> Result<Self, PastBound> {
        let bytes = state.decisions.values();
        let bytes = bytes.map(|decision| decision.choice.len() + decision.reason.len());
        let bytes = bytes.map(|text| text.saturating_add(DECISION_BYTES));
        let built = bytes
            .fold(0, usize::saturating_add)
            .saturating_mul(BYTE_WORK);
        if built > bound {
            return Err(PastBound { work: built });
        }
        let (vectors, terms) = vectors::of(state);
        let mut postings = vec![Vec::new(); terms];
        for (decision, vector) in vectors.iter().enumerate() {
            for &(term, weight) in &vector.terms {
                if let Some(posting) = postings.get_mut(term) {
                    posting.push((decision, weight));
                }
            }
        }
        Ok(Self {
            names: state.decisions.keys().map(String::as_str).collect(),
            vectors,
            postings,
            built,
            bound,
        })
    }

    /// The page of likely duplicate pairs `among` from the probe `from`
    /// on, by name, at most `limit` pairs.
    ///
    /// A pair is on the page of the later-named of its decisions the pass
    /// probes. Over every pair each decision probes those before it.
    /// Touching some decisions, either that pass, its pairs filtered, or
    /// each of those decisions against every other but one of them after
    /// it, whichever costs less. The page takes its first probe whatever
    /// its work, and the next while the build's work and theirs stay within
    /// the bound. It ends before a probe whose pairs would pass `limit`, but
    /// for its first, of whose pairs it keeps the best `limit`. The probes
    /// run on the thread pool, and the kept pairs are the smallest keys of
    /// a total order, so they do not depend on the threads.
    pub(crate) fn page(&self, among: Among<'_>, from: Option<&str>, limit: usize) -> Page<'s> {
        let admitted = self.admitted(among);
        let (probing, works) = self.cheaper(admitted.as_deref());
        let start = from.map_or(0, |from| self.names.partition_point(|&name| name < from));
        let probes: Vec<usize> = (start..self.vectors.len())
            .filter(|&probe| probing.probes(probe))
            .collect();
        let work = |probe: &usize| works.get(*probe).copied().unwrap_or(0);
        let (mut spent, mut taken) = (self.built, 0);
        for (position, probe) in probes.iter().enumerate() {
            spent = spent.saturating_add(work(probe));
            if position > 0 && spent > self.bound {
                break;
            }
            taken = position + 1;
        }
        let run = probes.get(..taken).unwrap_or_default();
        let filter = match probing {
            Probing::Earlier => admitted.as_deref(),
            Probing::Admitted(_) => None,
        };
        let kept = self.run(run, probing, filter, limit);
        let left_out = probes.get(taken).map(|&probe| (probe, PageEnd::Work));
        let mut page = self.paged(kept, run, left_out, limit);
        page.probes = probes.len();
        page.work = probes
            .iter()
            .map(work)
            .fold(self.built, usize::saturating_add);
        page
    }

    /// Per decision, whether `among` admits it: `None` for every pair.
    fn admitted(&self, among: Among<'_>) -> Option<Vec<bool>> {
        let Among::Touching(names) = among else {
            return None;
        };
        let mut admitted = vec![false; self.vectors.len()];
        for name in names {
            if let Some(slot) = self
                .names
                .binary_search(name)
                .ok()
                .and_then(|decision| admitted.get_mut(decision))
            {
                *slot = true;
            }
        }
        Some(admitted)
    }

    /// The cheaper probing for the decisions `admitted`, every one when
    /// `None`, and each decision's work under it.
    fn cheaper<'a>(&self, admitted: Option<&'a [bool]>) -> (Probing<'a>, Vec<usize>) {
        let earlier = self.works(Probing::Earlier);
        let Some(admitted) = admitted else {
            return (Probing::Earlier, earlier);
        };
        let alone = self.works(Probing::Admitted(admitted));
        let sum = |works: &[usize]| {
            works
                .iter()
                .fold(0usize, |sum, &work| sum.saturating_add(work))
        };
        match sum(&alone) < sum(&earlier) {
            true => (Probing::Admitted(admitted), alone),
            false => (Probing::Earlier, earlier),
        }
    }

    /// Each decision's work as a probe of `probing`, 0 for one it does not
    /// probe: the posting entries it reads, its own included, and
    /// [`PAIR_WORK`] for each pair it may test, at most one per entry read
    /// and one per decision it reaches.
    fn works(&self, probing: Probing<'_>) -> Vec<usize> {
        let decisions = self.vectors.len();
        let mut entries = vec![0usize; decisions];
        for posting in &self.postings {
            for (position, &(decision, _)) in posting.iter().enumerate() {
                if let Some(read) = entries.get_mut(decision) {
                    *read = read.saturating_add(match probing {
                        // The entries before its own.
                        Probing::Earlier => position,
                        Probing::Admitted(_) => posting.len(),
                    });
                }
            }
        }
        let reach = |decision: usize| match probing {
            Probing::Earlier => decision,
            Probing::Admitted(_) => decisions.saturating_sub(1),
        };
        let work = |(decision, read): (usize, usize)| match probing.probes(decision) {
            true => read.saturating_add(read.min(reach(decision)).saturating_mul(PAIR_WORK)),
            false => 0,
        };
        entries.into_iter().enumerate().map(work).collect()
    }

    /// Run `probes` on the thread pool: the first `limit` + 1 pairs they
    /// offer by the selection's order, and each probe's count of pairs.
    fn run(
        &self,
        probes: &[usize],
        probing: Probing<'_>,
        filter: Option<&[bool]>,
        limit: usize,
    ) -> Kept {
        let decisions = self.vectors.len();
        let kept = limit.saturating_add(1);
        probes
            .par_iter()
            .with_min_len(PER_TASK)
            .fold(
                || Task::new(decisions, kept),
                |mut task, &probe| {
                    self.probe(probe, probing, filter, &mut task);
                    task
                },
            )
            .map(|task| Kept {
                best: task.best,
                counts: task.counts,
            })
            .reduce(|| Kept::new(kept), Kept::merge)
    }

    /// Offer to the task's selection each likely duplicate the probe
    /// `probe` finds, of a decision `filter` admits, when it has one.
    fn probe(&self, probe: usize, probing: Probing<'_>, filter: Option<&[bool]>, task: &mut Task) {
        let Task {
            scratch,
            best,
            counts,
        } = task;
        let Some(vector) = self.vectors.get(probe) else {
            return;
        };
        let (before, offered_by) = match probing {
            Probing::Earlier => (probe, None),
            Probing::Admitted(admitted) => (self.vectors.len(), Some(admitted)),
        };
        let admits = |decision: usize, admitted: &[bool]| admitted.get(decision) == Some(&true);
        let mut found = 0;
        scratch.dots(&self.postings, vector, before, |other, dot| {
            // A later admitted decision offers the pair on its own page.
            let later = offered_by.is_some_and(|admitted| other > probe && admits(other, admitted));
            let filtered =
                filter.is_some_and(|admitted| !admits(other, admitted) && !admits(probe, admitted));
            if other == probe || later || filtered {
                return;
            }
            if let Some(score) = self
                .vectors
                .get(other)
                .and_then(|held| likely(vector, held, dot))
            {
                found += 1;
                best.offer((probe, Reverse(score), other));
            }
        });
        counts.push((probe, found));
    }

    /// The page of `kept`, the pairs of the probes `run` ran, ending at
    /// `left_out` or before a probe whose pairs pass `limit`.
    fn paged(
        &self,
        kept: Kept,
        run: &[usize],
        left_out: Option<(usize, PageEnd)>,
        limit: usize,
    ) -> Page<'s> {
        let Kept { best, mut counts } = kept;
        let (_, mut keys) = best.into_sorted();
        counts.sort_unstable();
        let (ends, total) = within(&mut keys, &counts, run, limit);
        let next = ends.map(|end| (end, PageEnd::Pairs)).or(left_out);
        let end = next.map_or(usize::MAX, |(probe, _)| probe);
        let mut pairs: Vec<Pair<'s>> = keys
            .into_iter()
            .filter_map(|(probe, Reverse(score), other)| {
                Some(Pair {
                    first: self.names.get(probe.min(other))?,
                    second: self.names.get(probe.max(other))?,
                    score,
                    probe: self.names.get(probe)?,
                })
            })
            .collect();
        pairs.sort_by(|a, b| {
            (Reverse(a.score), a.first, a.second).cmp(&(Reverse(b.score), b.first, b.second))
        });
        Page {
            pairs,
            total,
            run: run
                .iter()
                .take_while(|&&probe| probe < end)
                .filter_map(|&probe| self.names.get(probe).copied())
                .collect(),
            probes: run.len(),
            next: next.and_then(|(probe, why)| Some((*self.names.get(probe)?, why))),
            work: 0,
        }
    }

    /// The likely duplicates of each decision of `names`, in their order:
    /// best first, then by name, at most `count`; none for a name the graph
    /// lacks. Refused, with the work, when the build's and the probes' work
    /// passes the bound: each probe reads every entry of its terms'
    /// postings, and may test one pair per entry and per other decision.
    pub(crate) fn nearest(
        &self,
        names: &[&str],
        count: usize,
    ) -> Result<Vec<Vec<Neighbour<'s>>>, PastBound> {
        let decisions = self.vectors.len();
        let held = |term: usize| self.postings.get(term).map_or(0, Vec::len);
        let probe_work = |name: &&str| {
            let Ok(decision) = self.names.binary_search(name) else {
                return 0;
            };
            let terms = self.vectors.get(decision).map_or(&[][..], |v| &v.terms[..]);
            let read = terms.iter().map(|&(term, _)| held(term)).sum::<usize>();
            let tested = read.min(decisions.saturating_sub(1));
            read.saturating_add(tested.saturating_mul(PAIR_WORK))
        };
        let work = names
            .iter()
            .map(probe_work)
            .fold(self.built, usize::saturating_add);
        if work > self.bound {
            return Err(PastBound { work });
        }
        let mut scratch = Scratch::new(decisions);
        Ok(names
            .iter()
            .map(|name| self.nearest_to(&mut scratch, name, count))
            .collect())
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

impl Probing<'_> {
    /// Whether `decision` is a probe.
    fn probes(self, decision: usize) -> bool {
        match self {
            Self::Earlier => true,
            Self::Admitted(admitted) => admitted.get(decision) == Some(&true),
        }
    }
}

/// Cut `keys`, the first `limit` + 1 pairs of the probes `run` ran by
/// probe, to the probes whose pairs all fit `limit`: the probe the page then
/// ends before, when it ends before the last, and the count of the pairs
/// kept probes found, of `counts`. A first probe whose own pairs pass
/// `limit` keeps its best `limit`, and the page ends after it.
fn within(
    keys: &mut Vec<PairKey>,
    counts: &[(usize, usize)],
    run: &[usize],
    limit: usize,
) -> (Option<usize>, usize) {
    let sum = |before: Option<usize>| {
        let kept = counts
            .iter()
            .filter(|&&(probe, _)| before.is_none_or(|end| probe < end));
        kept.map(|&(_, count)| count).sum()
    };
    let Some(&(cut, ..)) = keys.get(limit) else {
        return (None, sum(None));
    };
    let ends = match run.first() == Some(&cut) {
        true => run.iter().copied().find(|&probe| probe > cut),
        false => Some(cut),
    };
    let first = run.first() == Some(&cut);
    keys.retain(|&(probe, ..)| probe < cut || first);
    keys.truncate(limit);
    (ends, sum(ends))
}

/// The score of `a` and `b`, whose dot product is `dot`, when they share a
/// term and their cosine is [`Score::THRESHOLD`], 1/2, or above: `(2
/// dot)^2 >= |a|^2 |b|^2`, exact in 256 bits.
fn likely(a: &Vector, b: &Vector, dot: u128) -> Option<Score> {
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

/// What one task of the pairs pass holds over its run of probes.
struct Task {
    scratch: Scratch,
    best: Best<PairKey>,
    /// Each probe run and the count of the pairs it found.
    counts: Vec<(usize, usize)>,
}

impl Task {
    fn new(decisions: usize, limit: usize) -> Self {
        Self {
            scratch: Scratch::new(decisions),
            best: Best::new(limit),
            counts: Vec::new(),
        }
    }
}

/// What the tasks of a pass keep: the selection, and each probe's count.
struct Kept {
    best: Best<PairKey>,
    counts: Vec<(usize, usize)>,
}

impl Kept {
    const fn new(limit: usize) -> Self {
        Self {
            best: Best::new(limit),
            counts: Vec::new(),
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.best = self.best.merge(other.best);
        self.counts.extend(other.counts);
        self
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
