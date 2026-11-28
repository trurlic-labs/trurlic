//! Docs as the index reads them: this first paragraph is the file's,
//! its lines joined by one space.
//!
//! A second paragraph the index leaves out.

/// A journal of entries.
///
/// What follows the first paragraph is not held.
pub struct Journal {
    /// The entries, oldest first.
    pub entries: Vec<u32>,
    count: u32,
}

/**
 * A balance, written as a block doc whose lines
 * each start with a star.
 */
#[derive(Debug)]
pub enum Balance {
    /// Nothing owed.
    Even,
    Owed(u32),
}

#[doc = "An audit, documented by an attribute,"]
// A plain comment between the two is no part of the doc.
/// and by a line after it.
pub fn audit() {}

pub fn undocumented() {}

/// A first paragraph that runs on past the bound of the index, which
/// holds its first bytes and marks the doc as cut. The rest of these
/// lines is left out of the text, whatever it says: a reader of the map
/// sees the start of the paragraph and that it goes on. The paragraph
/// goes on here so that it passes the bound with room to spare, as a
/// person who documents an item at length writes it, one sentence after
/// another, each adding a detail the item's name and signature cannot
/// state. Then a last sentence, still part of the one paragraph, carries
/// it well past the bound and ends it.
pub fn verbose() {}

/// The terms of the journal.
pub mod terms {
    //! Its inner doc follows the outer one.
}
