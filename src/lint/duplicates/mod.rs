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
//! term, and none allocates: the pass counts every pair and keeps only the
//! best it is asked for (`best`), however many there are. A term held by
//! most decisions makes the pass visit most pairs, so its cost grows with
//! the square of the decisions, and each probe's work is counted from the
//! postings and charged against a bound before it runs
//! ([`Similarity::pairs`]).
//!
//! The index's build and [`Similarity::nearest`] charge no bound: neither
//! costs more than a fixed multiple of the bytes the call's load of the
//! graph read and parsed. A decision's text is no longer than the node
//! file it was parsed from, and a term is two bytes or more and ends at a
//! byte no term holds, so a text of `b` bytes puts at most (b + 1) / 3
//! entries in the postings. The build reads each byte of the texts once.
//! `nearest` probes the decisions one batch wrote, at most `MAX_BATCH_OPS`
//! (50), each reading at most every entry and testing at most one pair per
//! entry it reads: at most 100 steps an entry, about 34 a byte of the
//! texts. Only the pairs pass grows with the square of the decisions.

mod best;
mod fixed;
mod score;
mod similarity;
mod terms;
mod vectors;

pub(crate) use score::Score;
pub(crate) use similarity::{Among, Similarity};

#[cfg(test)]
mod tests;
