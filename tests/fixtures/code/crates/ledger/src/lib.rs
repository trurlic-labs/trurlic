//! Mentions `fn vanished` and `struct Phantom` in a comment only.

use std::fmt;

use crate::books::Imported;

/// The most entries a page holds.
pub const PAGE_SIZE: usize = 64;
static mut OPENED: u32 = 0;

pub(crate) const fn page_count() -> char {
    '{'
}

/// One per kind of literal, each hashed with its prefix and delimiters.
pub const SPELLINGS: [&[u8]; 4] = [b"abc", br"abc", br#"abc"#, &[b'a']];

macro_rules! tally {
    () => {
        fn helper_in_a_template() {}
    };
}

/// A book of entries.
#[derive(Debug)]
pub struct Book {
    pub title: String,
    pages: u32,
}

pub struct Weight(u32);

/// The count a page holds.
pub type PageCount = u32;

pub union Raw {
    bits: u32,
    float: f32,
}

pub enum Entry {
    Blank,
    Amount(u64),
    Transfer { from: u32, to: u32 },
}

pub trait Audit {
    type Report;
    const DEPTH: u8;
    fn audit(&self) -> Self::Report;
    fn summary(&self) -> String {
        String::new()
    }
}

impl Book {
    pub fn new(title: String) -> Self {
        Self { title, pages: 0 }
    }

    pub fn pages(&self) -> u32 {
        self.pages
    }
}

impl fmt::Display for Book {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("}")
    }
}

impl fmt::Debug for Weight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Entry> for Book {
    fn from(entry: Entry) -> Self {
        let _ = entry;
        Book::new(String::new())
    }
}

impl From<Weight> for Book {
    fn from(weight: Weight) -> Self {
        let _ = weight;
        Book::new(String::new())
    }
}

impl<'a> Audit for &'a mut Book {
    type Report = u32;
    const DEPTH: u8 = 1;
    fn audit(&self) -> u32 {
        self.pages
    }
}

impl Book {
    pub fn close(self) {}
}

pub mod books;
mod marks;
mod rules;
#[cfg(test)]
mod checks;

mod archive {
    pub fn seal() {}

    pub struct Book;
}

fn reconcile(entries: &[Entry]) -> usize {
    fn weigh(entry: &Entry) -> usize {
        let _ = entry;
        1
    }
    let moved = entries.iter().map(weigh).sum();
    tally!();
    moved
}

pub use crate::books::{Catalog, Shelf as Rack, self};
pub use crate::books::*;
pub(crate) use tally;

extern "C" {
    fn external_total() -> u32;
}

define! {
    /// A view spelled inside an invocation.
    pub(crate) struct LedgerView<'a> {
        book: &'a Book,
    }
}

thread_local! {
    static CURRENT: u32 = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile() {
        assert_eq!(super::reconcile(&[]), 0);
    }
}
