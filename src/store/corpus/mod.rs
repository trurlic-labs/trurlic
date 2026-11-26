//! A seeded synthetic project for benches and property tests: a store of
//! [`Corpus::decisions`] decisions shaped like a production graph, and a
//! Rust source tree that their anchors resolve in.
//!
//! The store is written in one journaled commit of canonical files, the
//! path every write takes, and its tests hold it valid. Every draw comes
//! from the seeded generator (`seeded.rs`): benches compare commits only
//! while the corpus stays byte-identical across them. The project is drawn
//! in `project.rs`, its text in `prose.rs`, from the tables of `words.rs`.

mod project;
mod prose;
pub(crate) mod seeded;
mod words;

pub use project::Corpus;
