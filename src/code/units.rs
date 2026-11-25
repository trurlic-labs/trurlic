//! The units of a tree's span hashes and the hashes over them. A tree's
//! tokens (`syntax.rs`) are its units, each glued kind of its grammar's
//! [`Lexicon`] two. A grammar whose layout carries syntax that no byte
//! holds (the statements and blocks of Python, whose NEWLINE, INDENT and
//! DEDENT are tokens of no width in no node) marks the nodes whose extent
//! is part of the hash: each such group opens and closes with a mark no
//! token's bytes give ([`Leaves::of_marked`]). A definition's hash is over
//! its units, each nested definition's units replaced by that
//! definition's hash, so each unit is hashed once however deep the
//! definitions nest ([`Leaves::hash_each`]).

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};

use tree_sitter::{Node, Tree};

use super::span::{CLOSE_MARK, NESTED_MARK, OPEN_MARK, SpanHash};
use super::syntax::{Lexicon, Take, Token, tokens, walk_tokens};

/// The units of a tree's span hashes, in order: each token's bytes, and
/// the marks that open and close a group of a marked tree.
pub(super) struct Leaves {
    units: Vec<Unit>,
}

/// One unit of a span hash: a token over bytes `from..to`, or a group's
/// mark, which holds no byte (`from` and `to` are where it stands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unit {
    from: usize,
    to: usize,
    mark: Mark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Text,
    Open,
    Close,
}

/// How the walk of a marked tree takes one node, by the general
/// resolver's query: the facts of a node no query names are all `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct NodeMarks {
    /// One token whole.
    pub(super) whole: bool,
    /// Left out of every hash.
    pub(super) skip: bool,
    /// Its extent is part of the hash: a mark opens and closes it.
    pub(super) group: bool,
    /// A definition's span: the units it holds are recorded.
    pub(super) span: bool,
}

impl NodeMarks {
    /// How the walk takes a node so marked.
    const fn take(self) -> Take {
        match (self.skip, self.whole) {
            (true, _) => Take::Skip,
            (false, true) => Take::Whole,
            (false, false) => Take::Walk,
        }
    }
}

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
    /// The tokens of `tree`, of a grammar of `lexicon`.
    pub(super) fn of(tree: &Tree, text: &str, lexicon: &Lexicon) -> Self {
        let mut units = Vec::new();
        tokens(tree.root_node(), text, lexicon, |token| {
            push_text(&mut units, token, lexicon);
            ControlFlow::Continue(())
        });
        Self { units }
    }

    /// The units of `tree`, each node taken as `marks` says by its id, and
    /// the range of units each node marked a span holds, by its id.
    pub(super) fn of_marked(
        tree: &Tree,
        (text, lexicon): (&str, &Lexicon),
        marks: &BTreeMap<usize, NodeMarks>,
    ) -> (Self, BTreeMap<usize, Range<usize>>) {
        let mut units = Vec::new();
        let mut spans: BTreeMap<usize, Range<usize>> = BTreeMap::new();
        let marks_of = |node: Node<'_>| marks.get(&node.id()).copied().unwrap_or_default();
        let take = |node: Node<'_>| marks_of(node).take();
        walk_tokens(tree.root_node(), (text, lexicon), take, |token| {
            match token {
                Token::Node(_) | Token::Gap { .. } => push_text(&mut units, token, lexicon),
                Token::Enter(node) => {
                    let marked = marks_of(node);
                    if marked.span {
                        spans.insert(node.id(), units.len()..units.len());
                    }
                    if marked.group {
                        units.push(Unit::mark(node.start_byte(), Mark::Open));
                    }
                }
                Token::Leave(node) => {
                    let marked = marks_of(node);
                    if marked.group {
                        units.push(Unit::mark(node.end_byte(), Mark::Close));
                    }
                    if let Some(range) = spans.get_mut(&node.id()).filter(|_| marked.span) {
                        range.end = units.len();
                    }
                }
            }
            ControlFlow::Continue(())
        });
        (Self { units }, spans)
    }

    /// The units within bytes `bytes`: each token wholly inside them. Read
    /// only of a tree with no marks, whose units hold bytes.
    pub(super) fn within(&self, bytes: &Range<usize>) -> Range<usize> {
        let first = self.units.partition_point(|unit| unit.from < bytes.start);
        let last = self.units.partition_point(|unit| unit.to <= bytes.end);
        first..last.max(first)
    }

    /// The hash of each of `ranges`, each a definition's units: over the
    /// units within it, in order, each token after its length and each
    /// mark as a length no token has, where each range within it comes in
    /// as its own hash instead of its units. A range the same as another
    /// is hashed alike; one that starts within another and ends past it
    /// is hashed as if it ended where that one does. Each unit goes into
    /// one hash, so the work is linear in the units and the ranges.
    pub(super) fn hash_each(&self, text: &str, ranges: &[Range<usize>]) -> Vec<SpanHash> {
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
}

/// Push the units of `token`, a token of bytes: one, or two for a glued
/// kind of `lexicon`.
fn push_text(units: &mut Vec<Unit>, token: Token<'_>, lexicon: &Lexicon) {
    let (from, to) = match token {
        Token::Node(node) => (node.start_byte(), node.end_byte()),
        Token::Gap { start, end } => (start, end),
        Token::Enter(_) | Token::Leave(_) => return,
    };
    let glued = match token {
        Token::Node(node) => lexicon
            .glued
            .iter()
            .find(|(kind, _)| *kind == node.kind())
            .map(|(_, first)| from + first)
            .filter(|split| *split < to),
        Token::Gap { .. } | Token::Enter(_) | Token::Leave(_) => None,
    };
    match glued {
        Some(split) => units.extend([Unit::text(from, split), Unit::text(split, to)]),
        None => units.push(Unit::text(from, to)),
    }
}

impl Unit {
    const fn text(from: usize, to: usize) -> Self {
        Self {
            from,
            to,
            mark: Mark::Text,
        }
    }

    const fn mark(at: usize, mark: Mark) -> Self {
        Self {
            from: at,
            to: at,
            mark,
        }
    }
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
                    Mark::Text => {
                        let token = self.text.as_bytes().get(unit.from..unit.to);
                        let token = token.unwrap_or_default();
                        top.units
                            .extend_from_slice(&(token.len() as u64).to_le_bytes());
                        top.units.extend_from_slice(token);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::rust::LEXICON;
    use crate::code::syntax::parse;

    fn rust_tree(text: &str) -> Tree {
        parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap()
    }

    /// The hash of the whole of `text`, a Rust file.
    fn hash(text: &str) -> SpanHash {
        let leaves = Leaves::of(&rust_tree(text), text, &LEXICON);
        let units = leaves.within(&(0..text.len()));
        leaves.hash_each(text, std::slice::from_ref(&units))[0]
    }

    #[test]
    fn a_hash_of_tokens_ignores_comments_and_layout() {
        let one = "fn f(a: u8) -> u8 { a + 1 }";
        let two = "/// Doc.\nfn f(\n    a: u8 // why\n) -> u8 {\n    a /* x */ + 1\n}";

        assert_eq!(hash(one), hash(two));
        assert_ne!(hash(one), hash("fn f(a: u8) -> u8 { a + 2 }"));
        assert_ne!(hash("fn f() { \"a b\" }"), hash("fn f() { \"a  b\" }"));
        let pattern = |p: &str| {
            hash(&format!(
                "fn f(x: i8) {{ match x {{ {p} => {{}} _ => {{}} }} }}"
            ))
        };
        assert_eq!(pattern("-1"), pattern("- 1"));
        assert_eq!(pattern("-1"), pattern("-/*c*/1"));
        assert_eq!(pattern("-1"), pattern("-\n 1"));
    }

    #[test]
    fn a_literal_is_hashed_with_its_prefix_and_delimiters() {
        let literals = [
            "r\"abc\"",
            "br\"abc\"",
            "r#\"abc\"#",
            "cr\"abc\"",
            "\"abc\"",
            "b\"abc\"",
            "c\"abc\"",
            "'a'",
            "b'a'",
            "1u8",
            "1i8",
        ];
        let hashes: std::collections::BTreeSet<SpanHash> = literals
            .iter()
            .map(|literal| hash(&format!("fn f() {{ let x = {literal}; }}")))
            .collect();

        assert_eq!(hashes.len(), literals.len());
    }

    #[test]
    fn a_byte_no_node_holds_is_a_token() {
        let separated = |separator: &str| {
            hash(&format!(
                "macro_rules! m {{ ($($x:ident){separator}*) => {{ $($x){separator}* }}; }}"
            ))
        };
        let recovered = |operator: &str| hash(&format!("fn f() {{ let x = 1 {operator} 2; }}"));

        assert_ne!(separated(","), separated(";"));
        assert_ne!(separated(","), separated(""));
        assert_ne!(recovered("@@"), recovered("\u{a7}"));
    }

    #[test]
    fn a_comment_among_bytes_no_node_holds_moves_no_hash() {
        assert_eq!(
            hash("macro_rules! m { ($($a:ident),*) => {}; }"),
            hash("macro_rules! m { ($($a:ident), /* c */ *) => {}; }")
        );
        assert_eq!(
            hash("macro_rules! m { ($a:ident) => {}; }"),
            hash("macro_rules! m { ($ /* c */ a:ident) => {}; }")
        );
        assert_eq!(
            hash("macro_rules! m { ($a:ident) => {}; }"),
            hash("macro_rules! m { ($ a:ident) => {}; }")
        );
        assert_eq!(
            hash("fn f() { let x = 1 @ @ 2; }"),
            hash("fn f() { let x = 1 @/* c /* nested */ */@ 2; }")
        );
    }

    #[test]
    fn a_nested_definition_enters_its_parents_hash_by_its_own() {
        let text = "mod a { fn b() { 1 } }\nmod a { fn b() { 2 } }\n";
        let leaves = Leaves::of(&rust_tree(text), text, &LEXICON);
        let ranges = [0..22, 8..20, 23..45, 31..43, 8..20].map(|bytes| leaves.within(&bytes));
        let hashes = leaves.hash_each(text, &ranges);

        assert_ne!(hashes[0], hashes[2], "an edit inside moves the parent");
        assert_ne!(hashes[1], hashes[3]);
        assert_eq!(hashes[1], hashes[4], "a range given twice hashes alike");
        let alone = "fn b() { 1 }";
        let alone_leaves = Leaves::of(&rust_tree(alone), alone, &LEXICON);
        let own = alone_leaves.hash_each(alone, &[alone_leaves.within(&(0..12))]);
        assert_eq!(
            hashes[1], own[0],
            "a hash is of its own tokens wherever it is"
        );
    }
}
