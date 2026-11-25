//! Parsing a file with a tree-sitter grammar, within a budget of work,
//! the tokens of its tree, and the span hashes of its definitions. Every
//! tree-sitter resolver (`rust.rs`) stands on these.
//!
//! A tree-sitter parse recovers from any input, and on hostile input its
//! error recovery can take many parser steps per byte, and its lexer can
//! scan the rest of the file again from each byte (an unclosed raw string
//! repeated). Both are counted: the parser reports every hundred steps,
//! and the lexer reads the text in chunks of [`CHUNK_BYTES`] through a
//! callback, so the bytes it scans are the bytes the callback serves. A
//! parse past [`PARSE_WORK_PER_BYTE`] steps, or bytes served, per byte of
//! the file is stopped and the file has no index. The bounds count work,
//! not time, so a file is refused on every machine or on none.
//!
//! A tree's tokens cover every byte of its file but comments and
//! whitespace: each leaf, each node of the grammar's literal kinds whole
//! (a string's prefix and delimiters, which a grammar may hide, are in
//! it), and each run of bytes the grammar keeps in no node (a
//! `macro_rules!` repetition's separator, what error recovery passes
//! over). A definition's hash is over its tokens, each nested
//! definition's tokens replaced by that definition's hash, so each token
//! is hashed once however deep the definitions nest ([`Leaves::hash_each`]).

use std::ops::{ControlFlow, Range};

use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use super::index::IndexFault;
use super::span::SpanHash;
use super::work::{PARSE_WORK_PER_BYTE, Work};

/// The steps between two of the parser's progress reports.
const STEPS_PER_REPORT: usize = 100;

/// The bytes the lexer is handed at a time.
const CHUNK_BYTES: usize = 1024;

/// The tree of `text` under `language`, or [`IndexFault::TooComplex`] when
/// the parse runs past its budget.
pub(super) fn parse(text: &str, language: &Language) -> Result<Tree, IndexFault> {
    let budget = || Work::of(text.len(), PARSE_WORK_PER_BYTE);
    parse_within(text, language, budget(), budget())
}

/// The tree of `text`, within the parser steps of `steps` and the bytes
/// scanned of `scanned`.
fn parse_within(
    text: &str,
    language: &Language,
    mut steps: Work,
    mut scanned: Work,
) -> Result<Tree, IndexFault> {
    let mut parser = Parser::new();
    // The one refusal is a grammar of an ABI this tree-sitter does not
    // read, and the grammars are pinned crates: it fails in a test or never.
    if parser.set_language(language).is_err() {
        return Err(IndexFault::Grammar);
    }
    let mut progress = |_: &_| match steps.spend(STEPS_PER_REPORT) {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(()),
    };
    let bytes = text.as_bytes();
    let mut within = true;
    // Past the budget the lexer is told the file has ended, so the parse
    // ends at once; the tree it gives is then discarded.
    let mut read = |offset: usize, _| {
        let rest = bytes.get(offset..).unwrap_or_default();
        let chunk = rest.get(..CHUNK_BYTES).unwrap_or(rest);
        within &= scanned.spend(chunk.len()).is_ok();
        if within { chunk } else { &[] }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(&mut read, None, Some(options));
    match tree {
        Some(tree) if within => Ok(tree),
        Some(_) | None => Err(IndexFault::TooComplex),
    }
}

/// One token of a tree: a leaf or a literal whole, or a run of bytes the
/// grammar keeps in no node.
#[derive(Debug, Clone, Copy)]
pub(super) enum Token<'n> {
    Node(Node<'n>),
    Gap(usize, usize),
}

/// Each token of `node`, in order, until `each` breaks: its leaves, each
/// node of a kind of `literals` whole, and each run of non-whitespace
/// bytes between a node's children that no child holds; comments and
/// every other extra but an error ([`is_comment`]), and tokens of no
/// width (the parser's inserted missing tokens), left out. One cursor walk: each node and each byte is
/// visited once.
pub(super) fn tokens<'n>(
    node: Node<'n>,
    text: &str,
    literals: &[&str],
    mut each: impl FnMut(Token<'n>) -> ControlFlow<()>,
) {
    let mut cursor = node.walk();
    // The end of the last child seen of each node whose children are
    // being walked, outermost first.
    let mut ends: Vec<usize> = Vec::new();
    loop {
        let current = cursor.node();
        if let Some(last) = ends.last_mut() {
            let gap = (*last, current.start_byte());
            *last = current.end_byte();
            if gaps(text, gap, &mut each).is_break() {
                return;
            }
        }
        let unit = current.child_count() == 0 || literals.contains(&current.kind());
        if !is_comment(current) {
            if !unit && cursor.goto_first_child() {
                ends.push(current.start_byte());
                continue;
            }
            if unit
                && current.end_byte() > current.start_byte()
                && each(Token::Node(current)).is_break()
            {
                return;
            }
        }
        if climb(&mut cursor, node, &mut ends, text, &mut each).is_break() {
            return;
        }
    }
}

/// Move `cursor` to the next node of `top`'s walk: a sibling, or the next
/// of an ancestor once the gap after its last child is out. Breaks at the
/// walk's end, or when `each` does.
fn climb<'n>(
    cursor: &mut tree_sitter::TreeCursor<'n>,
    top: Node<'n>,
    ends: &mut Vec<usize>,
    text: &str,
    each: &mut impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> ControlFlow<()> {
    loop {
        if cursor.node() == top {
            return ControlFlow::Break(());
        }
        if cursor.goto_next_sibling() {
            return ControlFlow::Continue(());
        }
        if !cursor.goto_parent() {
            return ControlFlow::Break(());
        }
        let parent = cursor.node();
        let last = ends.pop().unwrap_or(parent.end_byte());
        gaps(text, (last, parent.end_byte()), each)?;
    }
}

/// Whether `node` is left out of the tokens: an extra (a comment), but
/// not the error that recovery passes over, which the grammar may also
/// make an extra.
pub(super) fn is_comment(node: Node<'_>) -> bool {
    node.is_extra() && !node.is_error()
}

/// Each run of non-whitespace bytes of `text` within `from..to`, a gap
/// between two nodes, as a token.
fn gaps<'n>(
    text: &str,
    (from, to): (usize, usize),
    each: &mut impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let Some(gap) = text.get(from..to) else {
        return ControlFlow::Continue(());
    };
    let mut run: Option<usize> = None;
    for (at, c) in gap.char_indices() {
        match (c.is_whitespace(), run) {
            (true, Some(start)) => {
                each(Token::Gap(from + start, from + at))?;
                run = None;
            }
            (false, None) => run = Some(at),
            (true, None) | (false, Some(_)) => {}
        }
    }
    match run {
        Some(start) => each(Token::Gap(from + start, to)),
        None => ControlFlow::Continue(()),
    }
}

/// The byte ranges of a tree's tokens, in order.
pub(super) struct Leaves {
    ranges: Vec<(usize, usize)>,
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
/// in, the hash of each range found so far, and the next token to feed.
struct Hashing<'a> {
    leaves: &'a Leaves,
    text: &'a str,
    open: Vec<Open>,
    hashes: Vec<SpanHash>,
    next: usize,
}

impl Leaves {
    /// The tokens of `tree`, whose grammar's literal kinds are `literals`.
    pub(super) fn of(tree: &Tree, text: &str, literals: &[&str]) -> Self {
        let mut ranges = Vec::new();
        tokens(tree.root_node(), text, literals, |token| {
            ranges.push(match token {
                Token::Node(node) => (node.start_byte(), node.end_byte()),
                Token::Gap(from, to) => (from, to),
            });
            ControlFlow::Continue(())
        });
        Self { ranges }
    }

    /// The hash of each of `ranges`, a definition's bytes in `text`: over
    /// the tokens within it, in order, each with its length, where each
    /// range within it comes in as its own hash instead of its tokens. A
    /// range the same as another is hashed alike; one that starts within
    /// another and ends past it is hashed as if it ended where that one
    /// does. Each token goes into one hash, so the work is linear in the
    /// tokens and the ranges.
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

impl Hashing<'_> {
    /// Open the hash of range `at`, `range`, within the innermost open one:
    /// the same one again when it has the same bytes.
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

    /// Feed the tokens that start before `limit` into the innermost open
    /// hash that holds them, closing each that ends before a token does.
    fn feed_before(&mut self, limit: usize) {
        while let Some(&(from, to)) = self
            .leaves
            .ranges
            .get(self.next)
            .filter(|(from, _)| *from < limit)
        {
            while self.open.last().is_some_and(|top| top.end < to) {
                self.close();
            }
            if let Some(top) = self.open.last_mut() {
                let token = self.text.as_bytes().get(from..to).unwrap_or_default();
                top.units
                    .extend_from_slice(&(token.len() as u64).to_le_bytes());
                top.units.extend_from_slice(token);
            }
            self.next += 1;
        }
    }

    /// Close every open hash that ends at or before `at`.
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
            around.units.extend_from_slice(&u64::MAX.to_le_bytes());
            around.units.extend_from_slice(hash.as_bytes());
        }
    }
}

/// The identity of a grammar: its ABI, its version, the counts of its
/// symbols, fields and parse states, and its node types, hashed.
pub(super) fn grammar_identity(language: &Language, node_types: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    let version = language.metadata().map_or([0; 3], |found| {
        [
            found.major_version,
            found.minor_version,
            found.patch_version,
        ]
    });
    for count in [
        language.abi_version(),
        language.node_kind_count(),
        language.parse_state_count(),
        language.field_count(),
    ] {
        hasher.update(&(count as u64).to_le_bytes());
    }
    hasher.update(&version);
    hasher.update(node_types.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The text of `node` in `text`.
pub(super) fn text_of<'t>(node: Node<'_>, text: &'t str) -> &'t str {
    text.get(node.byte_range()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::rust::LITERALS;

    fn rust_tree(text: &str) -> Tree {
        parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap()
    }

    /// The hash of the whole of `text`, a Rust file.
    fn hash(text: &str) -> SpanHash {
        let leaves = Leaves::of(&rust_tree(text), text, &LITERALS);
        leaves.hash_each(text, std::slice::from_ref(&(0..text.len())))[0]
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

    /// Every Rust file under `src/`, and the fixture of macro
    /// repetitions and error recovery.
    fn token_corpus() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = vec![(
            "fixture".to_owned(),
            "macro_rules! m { ($($x:ident),*; $($y:expr);+) => { $($x)-+ }; }\nfn f() { let x = 1 @@ 2; }\n"
                .to_owned(),
        )];
        let mut pending = vec![root.join("src")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    found.push((path.display().to_string(), text));
                }
            }
        }
        found
    }

    #[test]
    fn every_byte_but_comments_and_whitespace_is_in_one_token() {
        for (file, text) in token_corpus() {
            let tree = rust_tree(&text);
            let mut covered = vec![false; text.len()];
            for &(from, to) in &Leaves::of(&tree, &text, &LITERALS).ranges {
                for byte in &mut covered[from..to] {
                    assert!(!*byte, "{file}: byte {from} in two tokens");
                    *byte = true;
                }
            }
            let mut comments = vec![false; text.len()];
            let mut cursor = tree.walk();
            'walk: loop {
                let node = cursor.node();
                if is_comment(node) {
                    comments[node.byte_range()].fill(true);
                } else if cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break 'walk;
                    }
                }
            }
            for (at, c) in text.char_indices() {
                let silent = c.is_whitespace() || comments[at];
                assert!(
                    covered[at] || silent,
                    "{file}: byte {at} ({c:?}) is in no token"
                );
            }
        }
    }

    #[test]
    fn a_nested_definition_enters_its_parents_hash_by_its_own() {
        let text = "mod a { fn b() { 1 } }\nmod a { fn b() { 2 } }\n";
        let leaves = Leaves::of(&rust_tree(text), text, &LITERALS);
        let hashes = leaves.hash_each(text, &[0..22, 8..20, 23..45, 31..43, 8..20]);

        assert_ne!(hashes[0], hashes[2], "an edit inside moves the parent");
        assert_ne!(hashes[1], hashes[3]);
        assert_eq!(hashes[1], hashes[4], "a range given twice hashes alike");
        let alone = "fn b() { 1 }";
        let own = Leaves::of(&rust_tree(alone), alone, &LITERALS)
            .hash_each(alone, std::slice::from_ref(&(0..12)));
        assert_eq!(
            hashes[1], own[0],
            "a hash is of its own tokens wherever it is"
        );
    }

    #[test]
    fn a_parse_past_its_budget_gives_no_tree() {
        let rust = tree_sitter_rust::LANGUAGE.into();
        let text = "fn f() {}\n".repeat(1_000);
        let budget = Work::units;

        assert!(parse_within(&text, &rust, budget(text.len() * 4), budget(text.len() * 4)).is_ok());
        assert_eq!(
            parse_within(&text, &rust, budget(text.len() / 2), budget(text.len() * 4)).err(),
            Some(IndexFault::TooComplex)
        );
        assert_eq!(
            parse_within(&text, &rust, budget(text.len() * 4), budget(text.len() / 2)).err(),
            Some(IndexFault::TooComplex)
        );
    }

    #[test]
    fn a_lexer_that_rescans_the_file_is_stopped_within_its_budget() {
        // Each unclosed raw string makes the lexer scan to the end again:
        // unbounded, this takes minutes.
        let hostile = "r#\"".repeat(100_000);

        assert_eq!(
            parse(&hostile, &tree_sitter_rust::LANGUAGE.into()).err(),
            Some(IndexFault::TooComplex)
        );
    }
}
