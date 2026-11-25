//! Parsing a file with a tree-sitter grammar, within a budget of work,
//! the tokens of its tree, and the span hashes of its definitions. Every
//! tree-sitter resolver (`rust.rs`, `general.rs`) stands on these.
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
//! over). A grammar whose layout carries syntax that no byte holds (the
//! statements and blocks of Python, whose NEWLINE, INDENT and DEDENT are
//! tokens of no width in no node) marks the nodes whose extent is part of
//! the hash: each such group opens and closes with a mark no token's bytes
//! give ([`Leaves::of_marked`]). A definition's hash is over its tokens,
//! each nested definition's tokens replaced by that definition's hash, so
//! each token is hashed once however deep the definitions nest
//! ([`Leaves::hash_each`]).

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};

use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use super::index::IndexFault;
use super::span::{CLOSE_MARK, NESTED_MARK, OPEN_MARK, SpanHash};
use super::work::{PARSE_WORK_PER_BYTE, Work};

/// The steps between two of the parser's progress reports.
pub(super) const STEPS_PER_REPORT: usize = 100;

/// The bytes the lexer is handed at a time. The lexer reads again within
/// the chunk it holds without asking for it, so a rescan inside one chunk
/// is uncounted: a small chunk keeps that uncounted rescan short.
pub(super) const CHUNK_BYTES: usize = 64;

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

/// One event of a walk of a tree's tokens: a leaf or a node taken whole,
/// a run of bytes the grammar keeps in no node, or the walk entering or
/// leaving a node it does not leave out.
#[derive(Debug, Clone, Copy)]
pub(super) enum Token<'n> {
    Node(Node<'n>),
    Gap { start: usize, end: usize },
    Enter(Node<'n>),
    Leave(Node<'n>),
}

impl Token<'_> {
    /// The byte a token of bytes starts at; `None` for an entry or a
    /// leaving, which holds none.
    fn start(&self) -> Option<usize> {
        match self {
            Self::Node(node) => Some(node.start_byte()),
            Self::Gap { start, .. } => Some(*start),
            Self::Enter(_) | Self::Leave(_) => None,
        }
    }
}

/// How a walk of the tokens takes a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Take {
    /// Its children are walked; a leaf is a token.
    Walk,
    /// It is one token, whatever it holds.
    Whole,
    /// It is left out, as a comment is.
    Skip,
}

/// Each token of `node`, in order, until `each` breaks: its leaves, each
/// node of a literal kind of `lexicon` whole, and each run of bytes
/// between a node's children that no child holds. Whitespace and the
/// comments `lexicon` delimits are left out. So are comments and every
/// other extra but an error ([`is_comment`]), and tokens of no width (the
/// parser's inserted missing tokens).
pub(super) fn tokens<'n>(
    node: Node<'n>,
    text: &str,
    lexicon: &Lexicon,
    each: impl FnMut(Token<'n>) -> ControlFlow<()>,
) {
    let take = |node: Node<'_>| match lexicon.literals.contains(&node.kind()) {
        true => Take::Whole,
        false => Take::Walk,
    };
    walk_tokens(node, (text, lexicon), take, each);
}

/// The walk of [`tokens`], each node taken as `take` says, and each node
/// it does not leave out entered and left. One cursor walk: each node and
/// each byte is visited once.
pub(super) fn walk_tokens<'n>(
    node: Node<'n>,
    (text, lexicon): (&str, &Lexicon),
    take: impl Fn(Node<'n>) -> Take,
    each: impl FnMut(Token<'n>) -> ControlFlow<()>,
) {
    let mut cursor = node.walk();
    // The end of the last child seen of each node whose children are
    // being walked, outermost first.
    let mut ends: Vec<usize> = Vec::new();
    let mut each = without_comments(text, lexicon, each);
    loop {
        let current = cursor.node();
        if let Some(last) = ends.last_mut() {
            let gap = (*last, current.start_byte());
            *last = current.end_byte();
            if gaps(text, gap, lexicon, &mut each).is_break() {
                return;
            }
        }
        let taken = match is_comment(current) {
            true => Take::Skip,
            false => take(current),
        };
        if taken != Take::Skip {
            if each(Token::Enter(current)).is_break() {
                return;
            }
            if taken == Take::Walk && cursor.goto_first_child() {
                ends.push(current.start_byte());
                continue;
            }
            if current.end_byte() > current.start_byte() && each(Token::Node(current)).is_break() {
                return;
            }
            if each(Token::Leave(current)).is_break() {
                return;
            }
        }
        if climb(&mut cursor, node, &mut ends, (text, lexicon), &mut each).is_break() {
            return;
        }
    }
}

/// `each`, passed over the tokens of a comment a grammar lexes as tokens
/// where it does not expect one (tree-sitter-rust after a repetition's
/// separator). A token that opens a comment by `lexicon` starts bytes no
/// token is taken from.
fn without_comments<'n>(
    text: &str,
    lexicon: &Lexicon,
    mut each: impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> impl FnMut(Token<'n>) -> ControlFlow<()> {
    let mut comment_end = 0;
    move |token: Token<'n>| {
        let Some(start) = token.start() else {
            return each(token);
        };
        if start < comment_end {
            return ControlFlow::Continue(());
        }
        match lexicon.comment(text.get(start..).unwrap_or_default()) {
            0 => each(token),
            length => {
                comment_end = start + length;
                ControlFlow::Continue(())
            }
        }
    }
}

/// Move `cursor` to the next node of `top`'s walk: a sibling, or the next
/// of an ancestor once the gap after its last child is out and the
/// ancestor is left. Breaks at the walk's end, or when `each` does.
fn climb<'n>(
    cursor: &mut tree_sitter::TreeCursor<'n>,
    top: Node<'n>,
    ends: &mut Vec<usize>,
    (text, lexicon): (&str, &Lexicon),
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
        gaps(text, (last, parent.end_byte()), lexicon, each)?;
        each(Token::Leave(parent))?;
    }
}

/// Whether `node` is left out of the tokens: an extra (a comment), but
/// not the error that recovery passes over, which the grammar may also
/// make an extra.
pub(super) fn is_comment(node: Node<'_>) -> bool {
    node.is_extra() && !node.is_error()
}

/// What a grammar's tokens are made of beside its tree. It names the node
/// kinds that are lexical literals, each one token whole. It names the
/// comment delimiters, by which a comment the grammar lexes as tokens, or
/// leaves among bytes no node holds, is dropped as whitespace is.
pub(super) struct Lexicon {
    pub(super) literals: &'static [&'static str],
    pub(super) line_comments: &'static [&'static str],
    /// Each opening and closing delimiter of a block comment.
    pub(super) block_comments: &'static [(&'static str, &'static str)],
    /// Whether a block comment holds another (Rust's do).
    pub(super) nested_blocks: bool,
    /// Leaf kinds the grammar lexes as one token where the language has
    /// two, with the bytes of the first (Rust's metavariable `$a`, which
    /// may be written `$ a`): hashed as two, so the space between moves
    /// nothing.
    pub(super) glued: &'static [(&'static str, usize)],
}

impl Lexicon {
    /// The bytes of the comment `rest` opens with; 0 when it opens with
    /// none. A comment not closed runs to the end of `rest`.
    fn comment(&self, rest: &str) -> usize {
        if let Some(open) = self
            .line_comments
            .iter()
            .find(|open| rest.starts_with(**open))
        {
            return rest.get(open.len()..).map_or(rest.len(), |after| {
                open.len() + after.find('\n').unwrap_or(after.len())
            });
        }
        let Some((open, close)) = self
            .block_comments
            .iter()
            .find(|(open, _)| rest.starts_with(*open))
        else {
            return 0;
        };
        let mut depth = 0_usize;
        let mut at = 0;
        while let Some(tail) = rest.get(at..).filter(|tail| !tail.is_empty()) {
            if tail.starts_with(close) {
                depth = depth.saturating_sub(1);
                at += close.len();
                if depth == 0 {
                    return at;
                }
            } else if tail.starts_with(open) && (depth == 0 || self.nested_blocks) {
                depth += 1;
                at += open.len();
            } else {
                at += tail.chars().next().map_or(1, char::len_utf8);
            }
        }
        rest.len()
    }
}

/// Each run of bytes of `text` within `from..to`, a gap between two
/// nodes, that is neither whitespace nor a comment of `lexicon`, as a
/// token.
fn gaps<'n>(
    text: &str,
    (from, to): (usize, usize),
    lexicon: &Lexicon,
    each: &mut impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let Some(gap) = text.get(from..to) else {
        return ControlFlow::Continue(());
    };
    let mut run: Option<usize> = None;
    let mut at = 0;
    while let Some(rest) = gap.get(at..).filter(|rest| !rest.is_empty()) {
        let comment = lexicon.comment(rest);
        let c = rest.chars().next().unwrap_or(' ');
        let silent = comment > 0 || c.is_whitespace();
        match (silent, run) {
            (true, Some(start)) => {
                each(Token::Gap {
                    start: from + start,
                    end: from + at,
                })?;
                run = None;
            }
            (false, None) => run = Some(at),
            (true, None) | (false, Some(_)) => {}
        }
        at += comment.max(c.len_utf8());
    }
    match run {
        Some(start) => each(Token::Gap {
            start: from + start,
            end: to,
        }),
        None => ControlFlow::Continue(()),
    }
}

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
    /// One token whole, unless a definition's span starts inside it.
    pub(super) whole: bool,
    /// Left out of every hash.
    pub(super) skip: bool,
    /// Its extent is part of the hash: a mark opens and closes it.
    pub(super) group: bool,
    /// A definition's span: the units it holds are recorded.
    pub(super) span: bool,
}

impl NodeMarks {
    /// How the walk takes `node`, so marked; a node marked whole that one
    /// of `span_starts` falls inside is walked.
    fn take(self, node: Node<'_>, span_starts: &[usize]) -> Take {
        let held = span_starts
            .get(span_starts.partition_point(|&at| at <= node.start_byte())..)
            .and_then(|after| after.first())
            .is_some_and(|&at| at < node.end_byte());
        match (self.skip, self.whole && !held) {
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
    /// the range of units each node marked a span holds, by its id. A
    /// node marked whole that a span starts inside is walked: a token
    /// never hides a definition. `span_starts` is sorted.
    pub(super) fn of_marked(
        tree: &Tree,
        (text, lexicon): (&str, &Lexicon),
        marks: &BTreeMap<usize, NodeMarks>,
        span_starts: &[usize],
    ) -> (Self, BTreeMap<usize, Range<usize>>) {
        let mut units = Vec::new();
        let mut spans: BTreeMap<usize, Range<usize>> = BTreeMap::new();
        let marks_of = |node: Node<'_>| marks.get(&node.id()).copied().unwrap_or_default();
        let take = |node: Node<'_>| marks_of(node).take(node, span_starts);
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
    use crate::code::rust::LEXICON;

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
            for unit in &Leaves::of(&tree, &text, &LEXICON).units {
                let (from, to) = (unit.from, unit.to);
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
