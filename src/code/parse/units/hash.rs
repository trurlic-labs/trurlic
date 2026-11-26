//! The hashes over a tree's units: each definition's over the units
//! within its range, each unit after its length and each group's mark as
//! a length no token has, and each nested definition's units replaced by
//! that definition's hash, so each unit is hashed once however deep the
//! definitions nest ([`Leaves::hash_each`]). A unit is hashed as its
//! bytes but the holes in it, or as its spelling (`spelling.rs`).

use std::borrow::Cow;
use std::ops::Range;

use super::leaves::Leaves;
use super::spelling;
use super::unit::{Mark, Unit};
use crate::code::index::{CLOSE_MARK, NESTED_MARK, OPEN_MARK, SpanHash};

/// A definition whose hash is being built: where it ends, the units of
/// its hash so far, and the indexes of the ranges it is the hash of.
struct Open {
    start: usize,
    end: usize,
    units: Vec<u8>,
    ranges: Vec<usize>,
}

/// One pass of [`Leaves::hash_each`]: the hashes open from the outermost
/// in, the hash of each range found so far, and the next unit to feed.
struct Hashing<'a> {
    leaves: &'a Leaves,
    text: &'a str,
    open: Vec<Open>,
    hashes: Vec<SpanHash>,
    next: usize,
}

impl Leaves {
    /// The hash of each of `ranges`, each a definition's units: over the
    /// units within it, in order, each token after its length and each
    /// mark as a length no token has, where each range within it comes in
    /// as its own hash instead of its units. A range the same as another
    /// is hashed alike; one that starts within another and ends past it
    /// is hashed as if it ended where that one does. Each unit goes into
    /// one hash, so the work is linear in the units and the ranges.
    pub(in crate::code) fn hash_each(&self, text: &str, ranges: &[Range<usize>]) -> Vec<SpanHash> {
        let mut order: Vec<(usize, &Range<usize>)> = ranges.iter().enumerate().collect();
        order.sort_by_key(|(at, range)| (range.start, std::cmp::Reverse(range.end), *at));
        let mut hashing = Hashing {
            leaves: self,
            text,
            open: Vec::new(),
            hashes: vec![SpanHash::from_bytes([0; 32]); ranges.len()],
            next: 0,
        };
        for (at, range) in order {
            hashing.feed_before(range.start);
            hashing.close_ended(range.start);
            hashing.open(at, range);
        }
        hashing.feed_before(usize::MAX);
        hashing.close_ended(usize::MAX);
        hashing.hashes
    }

    /// The bytes `unit` is hashed as after its length: a token's, but
    /// for the holes inside it, or a delimiter's, a string's text's or a
    /// literal's spelling.
    pub(super) fn bytes_of<'t>(&self, unit: &Unit, text: &'t str) -> Cow<'t, [u8]> {
        let bytes = text.as_bytes();
        let token = bytes.get(unit.from..unit.to).unwrap_or_default();
        let mut holes = inside(&self.holes, unit).peekable();
        match unit.mark {
            Mark::Delimiter => Cow::Owned(spelling::delimiter(token)),
            Mark::String => Cow::Owned(self.string_text(unit, bytes)),
            Mark::Literal => Cow::Owned(spelling::literal(token)),
            Mark::Text if holes.peek().is_none() => Cow::Borrowed(token),
            Mark::Text => {
                let mut kept = Vec::with_capacity(token.len());
                let mut at = unit.from;
                for hole in holes {
                    kept.extend_from_slice(bytes.get(at..hole.start).unwrap_or_default());
                    at = hole.end;
                }
                kept.extend_from_slice(bytes.get(at..unit.to).unwrap_or_default());
                Cow::Owned(kept)
            }
            Mark::Open | Mark::Close => Cow::Borrowed(&[]),
        }
    }

    /// How `unit`, a string's text of `bytes`, is hashed: its bytes but
    /// the holes in it, each escape in it spelled by what it means.
    fn string_text(&self, unit: &Unit, bytes: &[u8]) -> Vec<u8> {
        // Each cut of the text in order, and whether it is an escape.
        let mut cuts: Vec<(&Range<usize>, bool)> = inside(&self.holes, unit)
            .map(|hole| (hole, false))
            .chain(inside(&self.escapes, unit).map(|escape| (escape, true)))
            .collect();
        cuts.sort_unstable_by_key(|(cut, _)| cut.start);
        let mut spelled = Vec::with_capacity(unit.to.saturating_sub(unit.from));
        let mut at = unit.from;
        for (cut, is_escape) in cuts {
            spelled.extend_from_slice(bytes.get(at..cut.start).unwrap_or_default());
            if is_escape {
                let escape = bytes.get(cut.start..cut.end).unwrap_or_default();
                spelled.extend(spelling::escape(escape));
            }
            at = cut.end;
        }
        spelled.extend_from_slice(bytes.get(at..unit.to).unwrap_or_default());
        spelled
    }
}

/// The ranges of `ranges`, in order and apart, that lie inside `unit`.
fn inside<'r>(ranges: &'r [Range<usize>], unit: &Unit) -> impl Iterator<Item = &'r Range<usize>> {
    let first = ranges.partition_point(|range| range.end <= unit.from);
    let end = unit.to;
    ranges
        .get(first..)
        .unwrap_or_default()
        .iter()
        .take_while(move |range| range.start < end)
}

impl Hashing<'_> {
    /// Open the hash of range `at`, `range`, within the innermost open one:
    /// the same one again when it holds the same units.
    fn open(&mut self, at: usize, range: &Range<usize>) {
        let end = self
            .open
            .last()
            .map_or(range.end, |top| range.end.min(top.end));
        match self.open.last_mut() {
            Some(top) if top.start == range.start && top.end == end => top.ranges.push(at),
            Some(_) | None => self.open.push(Open {
                start: range.start,
                end,
                units: Vec::new(),
                ranges: vec![at],
            }),
        }
    }

    /// Feed the units before unit `limit` into the innermost open hash
    /// that holds each, closing each that ends before it.
    fn feed_before(&mut self, limit: usize) {
        while let Some(unit) = self
            .leaves
            .units
            .get(self.next)
            .filter(|_| self.next < limit)
        {
            self.close_ended(self.next);
            if let Some(top) = self.open.last_mut() {
                match unit.mark {
                    Mark::Text | Mark::Delimiter | Mark::String | Mark::Literal => {
                        let token = self.leaves.bytes_of(unit, self.text);
                        top.units
                            .extend_from_slice(&(token.len() as u64).to_le_bytes());
                        top.units.extend_from_slice(&token);
                    }
                    Mark::Open => top.units.extend_from_slice(&OPEN_MARK),
                    Mark::Close => top.units.extend_from_slice(&CLOSE_MARK),
                }
            }
            self.next += 1;
        }
    }

    /// Close every open hash that ends at or before unit `at`.
    fn close_ended(&mut self, at: usize) {
        while self.open.last().is_some_and(|top| top.end <= at) {
            self.close();
        }
    }

    /// Close the innermost open hash: its ranges get it, and the one
    /// around it takes it in as a unit, after a length mark no token has.
    fn close(&mut self) {
        let Some(done) = self.open.pop() else {
            return;
        };
        let hash = SpanHash::of_units(&done.units);
        for at in done.ranges {
            if let Some(slot) = self.hashes.get_mut(at) {
                *slot = hash;
            }
        }
        if let Some(around) = self.open.last_mut() {
            around.units.extend_from_slice(&NESTED_MARK);
            around.units.extend_from_slice(hash.as_bytes());
        }
    }
}
