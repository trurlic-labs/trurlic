//! The units of a tree's span hashes, in order, built by one walk of its
//! tokens ([`Leaves`]): each token's bytes as its query or lexicon marks
//! them, the marks of a marked tree's groups, and the bytes inside units
//! that no unit holds.

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};

use tree_sitter::{Node, Tree};

use super::jsx::{JsxText, push_chars};
use super::spelling;
use super::unit::{Mark, Unit};
use crate::code::parse::tokens::{Lexicon, Take, Token, tokens, walk_tokens};

/// The units of a tree's span hashes, in order: each token's bytes, and
/// the marks that open and close a group of a marked tree; the comments,
/// spliced line continuations and bytes between joined strings inside
/// units, in order, whose bytes no unit holds; and the escapes inside
/// string text, in order.
#[derive(Default)]
pub(in crate::code::parse) struct Leaves {
    pub(super) units: Vec<Unit>,
    pub(super) holes: Vec<Range<usize>>,
    pub(super) escapes: Vec<Range<usize>>,
    /// The JSX text the last node taken by its characters ended, while
    /// the units are built.
    jsx: Option<JsxText>,
}

/// How the walk of a marked tree takes one node, by the general
/// resolver's query: the facts of a node no query names are all `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::code::parse) struct NodeMarks {
    /// One token whole.
    pub(in crate::code::parse) whole: bool,
    /// One token per token its lexicon's lexer finds in it.
    pub(in crate::code::parse) tokens: bool,
    /// Its text as JSX renders it: a token a character, an empty one a
    /// space.
    pub(in crate::code::parse) chars: bool,
    /// One token, a string's delimiter, hashed by what it means.
    pub(in crate::code::parse) delimiter: bool,
    /// A string's start that continues the string before it in an
    /// implicit concatenation.
    pub(in crate::code::parse) join: bool,
    /// One token, a string's text, with the text that continues it.
    pub(in crate::code::parse) string: bool,
    /// An escape of a string's text, hashed by what it means.
    pub(in crate::code::parse) escape: bool,
    /// One token, a number, hashed by what it means.
    pub(in crate::code::parse) literal: bool,
    /// Left out of every hash.
    pub(in crate::code::parse) skip: bool,
    /// Its extent is part of the hash: a mark opens and closes it.
    pub(in crate::code::parse) group: bool,
    /// A definition's span starts at it: the units before it are counted.
    pub(in crate::code::parse) opens: bool,
    /// A definition's span ends at it: the units up to its end are
    /// counted.
    pub(in crate::code::parse) closes: bool,
}

impl NodeMarks {
    /// How the walk takes a node so marked; `None` for a node no hash
    /// mark names.
    const fn take(self) -> Option<Take> {
        match self {
            Self { skip: true, .. } => Some(Take::Skip),
            Self { tokens: true, .. }
            | Self { chars: true, .. }
            | Self {
                delimiter: true, ..
            }
            | Self { string: true, .. }
            | Self { literal: true, .. } => Some(Take::Token),
            Self { whole: true, .. } => Some(Take::Text),
            Self { group: true, .. } => Some(Take::Walk),
            Self { .. } => None,
        }
    }
}

/// Where the spans of a marked tree's definitions lie among its units: by
/// node id, the units before each node a span opens at, and the units up
/// to the end of each node a span closes at. A node the walk leaves out
/// is in neither.
#[derive(Debug, Default)]
pub(in crate::code::parse) struct SpanUnits {
    opened: BTreeMap<usize, usize>,
    closed: BTreeMap<usize, usize>,
}

impl SpanUnits {
    /// The units from node `first`'s start to node `last`'s end; none
    /// when the walk left either out.
    pub(in crate::code::parse) fn between(&self, first: usize, last: usize) -> Range<usize> {
        match (self.opened.get(&first), self.closed.get(&last)) {
            (Some(&start), Some(&end)) if start <= end => start..end,
            (Some(_) | None, _) => 0..0,
        }
    }
}

impl Leaves {
    /// The tokens of `tree`, of a grammar of `lexicon`, but the leaves that
    /// start at a byte of `silent`, a sorted list of tokens that carry no
    /// meaning.
    pub(in crate::code::parse) fn of(
        tree: &Tree,
        text: &str,
        lexicon: &Lexicon,
        silent: &[usize],
    ) -> Self {
        let mut leaves = Self::default();
        tokens(tree.root_node(), text, lexicon, |token| {
            let is_silent = match token {
                Token::Node(node) => silent.binary_search(&node.start_byte()).is_ok(),
                Token::Gap { .. }
                | Token::Run { .. }
                | Token::Hole { .. }
                | Token::Enter(_)
                | Token::Leave(_) => false,
            };
            if !is_silent {
                leaves.push(token, (text, lexicon));
            }
            ControlFlow::Continue(())
        });
        leaves
    }

    /// The units of `tree`, each node taken as `marks` says by its id, and
    /// where the spans its marks open and close lie among them.
    pub(in crate::code::parse) fn of_marked(
        tree: &Tree,
        (text, lexicon): (&str, &Lexicon),
        marks: &BTreeMap<usize, NodeMarks>,
    ) -> (Self, SpanUnits) {
        let mut leaves = Self::default();
        let mut spans = SpanUnits::default();
        let take = |node: Node<'_>| marks_of(marks, node).take();
        walk_tokens(tree.root_node(), (text, lexicon), take, |token| {
            match token {
                Token::Node(node) => leaves.push_marked(node, (text, lexicon), marks),
                Token::Gap { .. } | Token::Run { .. } | Token::Hole { .. } => {
                    leaves.push(token, (text, lexicon));
                }
                Token::Enter(node) => {
                    let (marked, units) = (marks_of(marks, node), &mut leaves.units);
                    if marked.opens {
                        spans.opened.insert(node.id(), units.len());
                    }
                    if marked.group {
                        units.push(Unit::mark(node.start_byte(), Mark::Open));
                    }
                }
                Token::Leave(node) => {
                    let (marked, units) = (marks_of(marks, node), &mut leaves.units);
                    if marked.group {
                        units.push(Unit::mark(node.end_byte(), Mark::Close));
                    }
                    if marked.closes {
                        spans.closed.insert(node.id(), units.len());
                    }
                }
            }
            ControlFlow::Continue(())
        });
        (leaves, spans)
    }

    /// Push the units of `node`, a token, as `marks` says by its id.
    fn push_marked(
        &mut self,
        node: Node<'_>,
        (text, lexicon): (&str, &Lexicon),
        marks: &BTreeMap<usize, NodeMarks>,
    ) {
        match marks_of(marks, node) {
            NodeMarks { tokens: true, .. } => self.push_tokens(node, (text, lexicon)),
            NodeMarks { chars: true, .. } => push_chars(&mut self.units, node, text, &mut self.jsx),
            NodeMarks { string: true, .. } => self.push_string(node, marks),
            NodeMarks {
                delimiter: true,
                join,
                ..
            } => self.push_delimiter(node, join, text),
            NodeMarks { literal: true, .. } => self.units.push(Unit::spelled(node, Mark::Literal)),
            NodeMarks { .. } => self.push(Token::Node(node), (text, lexicon)),
        }
    }

    /// Push the unit of `node`, a string's text, and each escape that
    /// `marks` marks among its children, or the node itself: a string's
    /// text and escapes may be siblings (TypeScript's).
    fn push_string(&mut self, node: Node<'_>, marks: &BTreeMap<usize, NodeMarks>) {
        let mut cursor = node.walk();
        let escapes = node
            .children(&mut cursor)
            .chain(std::iter::once(node))
            .filter(|child| marks_of(marks, *child).escape);
        self.escapes.extend(escapes.map(|child| child.byte_range()));
        self.push_string_text(node.start_byte(), node.end_byte());
    }

    /// Push a string's text over bytes `from..to`: the rest of the last
    /// unit when that is the text it continues, the bytes between them a
    /// hole. Only a string's end or a joined string continues a text.
    fn push_string_text(&mut self, from: usize, to: usize) {
        match self.units.last_mut() {
            Some(last) if last.mark == Mark::String => {
                if last.to < from {
                    self.holes.push(last.to..from);
                }
                last.to = to;
            }
            Some(_) | None => self.units.push(Unit {
                from,
                to,
                mark: Mark::String,
            }),
        }
    }

    /// Push the units of `node`, a string's delimiter of `text`: the
    /// escapes a raw string's end holds before its quotes are the string's
    /// text, as written. A delimiter that `join`s a string to the one
    /// before it is left out, with that one's end, when the two start
    /// alike, so their texts are one.
    fn push_delimiter(&mut self, node: Node<'_>, join: bool, text: &str) {
        let (from, to) = (node.start_byte(), node.end_byte());
        let token = text.as_bytes().get(from..to).unwrap_or_default();
        if join && self.is_continued_by(token, text) {
            self.units.pop();
            return;
        }
        let quotes = from + spelling::escapes_before_quotes(token);
        if quotes > from {
            self.push_string_text(from, quotes);
        }
        self.units.push(Unit {
            from: quotes,
            to,
            mark: Mark::Delimiter,
        });
    }

    /// Whether a string whose start is `start` continues the one whose end
    /// is the last unit: that one's start, the unit before its text or
    /// before its end, spells as `start` does. The query joins no string
    /// with an `f` or `t` prefix, so one that starts alike has none either:
    /// its units are its start, its text if any, and its end.
    fn is_continued_by(&self, start: &[u8], text: &str) -> bool {
        let count = self.units.len();
        let before_end = count.checked_sub(2).and_then(|at| self.units.get(at));
        let first = match before_end {
            Some(unit) if unit.mark == Mark::String => count.checked_sub(3),
            Some(_) | None => count.checked_sub(2),
        };
        let ends = self
            .units
            .last()
            .is_some_and(|end| end.mark == Mark::Delimiter);
        first
            .and_then(|at| self.units.get(at))
            .is_some_and(|first| {
                let spelled = text
                    .as_bytes()
                    .get(first.from..first.to)
                    .unwrap_or_default();
                ends && first.mark == Mark::Delimiter
                    && spelling::delimiter(spelled) == spelling::delimiter(start)
            })
    }

    /// Push the units of `token`, a token of bytes of `text`, or the hole
    /// it is.
    fn push(&mut self, token: Token<'_>, (text, lexicon): (&str, &Lexicon)) {
        match token {
            Token::Hole { start, end } => self.holes.push(start..end),
            Token::Node(node) if self.is_spliced(node, (text, lexicon)) => {}
            Token::Node(_) | Token::Gap { .. } | Token::Run { .. } => {
                push_text(&mut self.units, token, lexicon);
            }
            Token::Enter(_) | Token::Leave(_) => {}
        }
    }

    /// Whether `node` is the rest of the last unit, as the preprocessor
    /// splices it: only a line continuation `lexicon` splices parts them,
    /// a word's character on each side. The unit then runs to the node's
    /// end, the continuation a hole of it.
    fn is_spliced(&mut self, node: Node<'_>, (text, lexicon): (&str, &Lexicon)) -> bool {
        let Some(last) = self.units.last_mut() else {
            return false;
        };
        if last.mark != Mark::Text || !lexicon.joins(text, last.to, node.start_byte()) {
            return false;
        }
        self.holes.push(last.to..node.start_byte());
        last.to = node.end_byte();
        true
    }

    /// Push a unit for each token `lexicon`'s lexer finds in `node`'s
    /// text, and a hole for each line continuation it splices inside one;
    /// the node whole when the lexicon has no lexer.
    fn push_tokens(&mut self, node: Node<'_>, (text, lexicon): (&str, &Lexicon)) {
        let start = node.start_byte();
        let Some(lex) = lexicon.tokens else {
            self.units.push(Unit::text(start, node.end_byte()));
            return;
        };
        let body = text.get(node.byte_range()).unwrap_or_default();
        lex(body, &mut |range| {
            let bytes = start + range.start..start + range.end;
            self.holes
                .extend(lexicon.spliced(text, bytes.start..bytes.end));
            self.units.push(Unit::text(bytes.start, bytes.end));
        });
    }

    /// The units within bytes `bytes`: each token wholly inside them. Read
    /// only of a tree with no marks, whose units hold bytes.
    pub(in crate::code::parse) fn within(&self, bytes: &Range<usize>) -> Range<usize> {
        let first = self.units.partition_point(|unit| unit.from < bytes.start);
        let last = self.units.partition_point(|unit| unit.to <= bytes.end);
        first..last.max(first)
    }

    /// The units within bytes `bytes` of `text`, each as it is hashed, a
    /// space between each two. Read only of a tree with no marks.
    pub(in crate::code::parse) fn spelled(&self, text: &str, bytes: &Range<usize>) -> String {
        let units = self.units.get(self.within(bytes)).unwrap_or_default();
        let tokens: Vec<String> = units
            .iter()
            .map(|unit| String::from_utf8_lossy(&self.bytes_of(unit, text)).into_owned())
            .collect();
        tokens.join(" ")
    }
}

/// Push the units of `token`, a token of bytes: one, or two for a glued
/// kind of `lexicon`.
fn push_text(units: &mut Vec<Unit>, token: Token<'_>, lexicon: &Lexicon) {
    let (from, to) = match token {
        Token::Node(node) => (node.start_byte(), node.end_byte()),
        Token::Gap { start, end } | Token::Run { start, end } => (start, end),
        Token::Hole { .. } | Token::Enter(_) | Token::Leave(_) => return,
    };
    let glued = match token {
        Token::Node(node) => lexicon
            .glued
            .iter()
            .find(|(kind, _)| *kind == node.kind())
            .map(|(_, first)| from + first)
            .filter(|split| *split < to),
        Token::Gap { .. }
        | Token::Run { .. }
        | Token::Hole { .. }
        | Token::Enter(_)
        | Token::Leave(_) => None,
    };
    match glued {
        Some(split) => units.extend([Unit::text(from, split), Unit::text(split, to)]),
        None => units.push(Unit::text(from, to)),
    }
}

/// How `node` is marked, by its id in `marks`; a node no mark names is
/// marked as no capture names it.
fn marks_of(marks: &BTreeMap<usize, NodeMarks>, node: Node<'_>) -> NodeMarks {
    marks.get(&node.id()).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::SpanHash;
    use crate::code::parse::rust::LEXICON;
    use crate::code::parse::syntax::parse;

    fn rust_tree(text: &str) -> Tree {
        parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap()
    }

    /// The hash of the whole of `text`, a Rust file.
    fn hash(text: &str) -> SpanHash {
        let leaves = Leaves::of(&rust_tree(text), text, &LEXICON, &[]);
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
        let leaves = Leaves::of(&rust_tree(text), text, &LEXICON, &[]);
        let ranges = [0..22, 8..20, 23..45, 31..43, 8..20].map(|bytes| leaves.within(&bytes));
        let hashes = leaves.hash_each(text, &ranges);

        assert_ne!(hashes[0], hashes[2], "an edit inside moves the parent");
        assert_ne!(hashes[1], hashes[3]);
        assert_eq!(hashes[1], hashes[4], "a range given twice hashes alike");
        let alone = "fn b() { 1 }";
        let alone_leaves = Leaves::of(&rust_tree(alone), alone, &LEXICON, &[]);
        let own = alone_leaves.hash_each(alone, &[alone_leaves.within(&(0..12))]);
        assert_eq!(
            hashes[1], own[0],
            "a hash is of its own tokens wherever it is"
        );
    }
}
