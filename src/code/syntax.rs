//! Parsing a file with a tree-sitter grammar, within a budget of work,
//! and the tokens of its tree. Every
//! tree-sitter resolver (`rust.rs`, `general.rs`) stands on these.
//!
//! A tree-sitter parse recovers from any input, and on hostile input its
//! lexer can scan the rest of the file again from each byte (an unclosed
//! raw string repeated). The lexer reads the text in chunks of
//! [`CHUNK_BYTES`] through a callback, so the bytes it scans are the bytes
//! the callback serves. A parse past [`PARSE_WORK_PER_BYTE`] bytes served
//! per byte of the file is stopped and the file has no index. The bound
//! counts work, not time, so a file is refused on every machine or on
//! none. The parser's own steps are not counted: tree-sitter keeps a
//! bounded number of stack versions, so its steps grow with the tokens,
//! and the time its error recovery takes is bounded by the size cap.
//!
//! A tree's tokens cover every byte of its file but comments and
//! whitespace: each leaf, each node of the grammar's literal kinds whole
//! (a string's prefix and delimiters, which a grammar may hide, are in
//! it), and each run of bytes the grammar keeps in no node (a
//! `macro_rules!` repetition's separator, what error recovery passes
//! over). A node a query takes as text is one token of its bytes but the
//! comments inside, cut at each child the query marks as well
//! ([`Take::Text`]). The span hashes over them are `units.rs`'s.

use std::ops::ControlFlow;

use tree_sitter::{Language, Node, Parser, Tree};

use super::index::IndexFault;
use super::work::{PARSE_WORK_PER_BYTE, Work};

/// The bytes the lexer is handed at a time. The lexer reads again within
/// the chunk it holds without asking for it, so a rescan inside one chunk
/// is uncounted: a small chunk keeps that uncounted rescan short.
pub(super) const CHUNK_BYTES: usize = 64;

/// The tree of `text` under `language`, or [`IndexFault::TooComplex`] when
/// the parse runs past its budget.
pub(super) fn parse(text: &str, language: &Language) -> Result<Tree, IndexFault> {
    parse_within(text, language, Work::of(text.len(), PARSE_WORK_PER_BYTE))
}

/// The tree of `text`, within the bytes its lexer is served of `scanned`.
fn parse_within(text: &str, language: &Language, mut scanned: Work) -> Result<Tree, IndexFault> {
    let mut parser = Parser::new();
    // The one refusal is a grammar of an ABI this tree-sitter does not
    // read, and the grammars are pinned crates: it fails in a test or never.
    if parser.set_language(language).is_err() {
        return Err(IndexFault::Grammar);
    }
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
    let tree = parser.parse_with_options(&mut read, None, None);
    match tree {
        Some(tree) if within => Ok(tree),
        Some(_) | None => Err(IndexFault::TooComplex),
    }
}

/// One event of a walk of a tree's tokens: a leaf or a node taken whole,
/// a run of bytes the grammar keeps in no node, a run of a node taken as
/// text and a comment inside it, or the walk entering or leaving a node it
/// does not leave out.
#[derive(Debug, Clone, Copy)]
pub(super) enum Token<'n> {
    Node(Node<'n>),
    Gap {
        start: usize,
        end: usize,
    },
    /// Bytes of a node taken as [`Take::Text`], one token without the
    /// bytes of the holes inside them.
    Run {
        start: usize,
        end: usize,
    },
    /// A comment inside a run: it comes before the run's token.
    Hole {
        start: usize,
        end: usize,
    },
    Enter(Node<'n>),
    Leave(Node<'n>),
}

impl Token<'_> {
    /// The byte a token of bytes starts at; `None` for a hole, an entry or
    /// a leaving, which is no token of bytes.
    fn start(&self) -> Option<usize> {
        match self {
            Self::Node(node) => Some(node.start_byte()),
            Self::Gap { start, .. } | Self::Run { start, .. } => Some(*start),
            Self::Hole { .. } | Self::Enter(_) | Self::Leave(_) => None,
        }
    }
}

/// How a walk of the tokens takes a node it marks; a node with no mark
/// is walked, and inside a node taken as text is part of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Take {
    /// Its children are walked; a leaf is a token.
    Walk,
    /// It is one token, whatever it holds.
    Token,
    /// Its text is one token, the comments in it left out. Each of its
    /// children with a mark is taken by it, cutting the text in two.
    Text,
    /// It is left out, as a comment is.
    Skip,
}

/// How the walk takes the children of a node it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inside {
    /// Each child by its mark, and each run of bytes after `last` that no
    /// child holds as tokens.
    Walked { last: usize },
    /// A node taken as text, whose run since the last cut starts at `run`.
    Text { run: usize },
    /// A node of the text of one around it, every byte of it in the text.
    Within,
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
    let take = |node: Node<'_>| {
        lexicon
            .literals
            .contains(&node.kind())
            .then_some(Take::Token)
    };
    walk_tokens(node, (text, lexicon), take, each);
}

/// The walk of [`tokens`], each node taken as `take` marks it, and each
/// node it does not leave out entered and left, but the nodes of a text.
/// One cursor walk: each node and each byte is visited once.
pub(super) fn walk_tokens<'n>(
    node: Node<'n>,
    (text, lexicon): (&str, &Lexicon),
    take: impl Fn(Node<'n>) -> Option<Take>,
    each: impl FnMut(Token<'n>) -> ControlFlow<()>,
) {
    let mut cursor = node.walk();
    // How the children of each node being walked are taken, outermost
    // first.
    let mut levels: Vec<Inside> = Vec::new();
    let mut each = without_comments(text, lexicon, each);
    loop {
        let current = cursor.node();
        let visited = visit(
            current,
            levels.last_mut(),
            (text, lexicon),
            &take,
            &mut each,
        );
        match visited {
            ControlFlow::Break(()) => return,
            ControlFlow::Continue(Some(inside)) if cursor.goto_first_child() => {
                levels.push(inside);
                continue;
            }
            ControlFlow::Continue(_) => {}
        }
        if climb(&mut cursor, node, &mut levels, (text, lexicon), &mut each).is_break() {
            return;
        }
    }
}

/// Take `node`, a child of a node taken as `inside` (`None` at the walk's
/// top): what it is goes to `each`, and how its children are taken comes
/// back, `None` when they are not walked.
fn visit<'n>(
    node: Node<'n>,
    inside: Option<&mut Inside>,
    (text, lexicon): (&str, &Lexicon),
    take: &impl Fn(Node<'n>) -> Option<Take>,
    each: &mut impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> ControlFlow<(), Option<Inside>> {
    let (start, end) = (node.start_byte(), node.end_byte());
    let mark = match is_comment(node) {
        true => Some(Take::Skip),
        false => take(node),
    };
    match (inside, mark) {
        (Some(Inside::Walked { last }), _) => {
            gaps(text, (*last, start), lexicon, each)?;
            *last = end;
        }
        (Some(Inside::Text { .. } | Inside::Within), _) if is_comment(node) => {
            each(Token::Hole { start, end })?;
            return ControlFlow::Continue(None);
        }
        (Some(Inside::Text { run }), Some(_)) => {
            if *run < start {
                each(Token::Run {
                    start: *run,
                    end: start,
                })?;
            }
            *run = end;
        }
        // A node with no mark of its own, or any node deeper than a
        // child: its bytes are the text's.
        (Some(Inside::Text { .. }), None) | (Some(Inside::Within), _) => {
            return ControlFlow::Continue((node.child_count() > 0).then_some(Inside::Within));
        }
        (None, _) => {}
    }
    open(node, mark, each)
}

/// Take `node`, marked `mark`, where a mark is taken by it: entered, and
/// a token itself or how its children are taken.
fn open<'n>(
    node: Node<'n>,
    mark: Option<Take>,
    each: &mut impl FnMut(Token<'n>) -> ControlFlow<()>,
) -> ControlFlow<(), Option<Inside>> {
    let start = node.start_byte();
    let inside = match mark {
        Some(Take::Skip) => return ControlFlow::Continue(None),
        Some(Take::Token) => None,
        Some(Take::Text) => Some(Inside::Text { run: start }),
        Some(Take::Walk) | None => Some(Inside::Walked { last: start }),
    };
    each(Token::Enter(node))?;
    if inside.is_some() && node.child_count() > 0 {
        return ControlFlow::Continue(inside);
    }
    if node.end_byte() > start {
        each(Token::Node(node))?;
    }
    each(Token::Leave(node))?;
    ControlFlow::Continue(None)
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
/// of an ancestor once what remains of it after its last child is out and
/// the ancestor is left. Breaks at the walk's end, or when `each` does.
fn climb<'n>(
    cursor: &mut tree_sitter::TreeCursor<'n>,
    top: Node<'n>,
    levels: &mut Vec<Inside>,
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
        let end = cursor.node().end_byte();
        match levels.pop() {
            Some(Inside::Walked { last }) => gaps(text, (last, end), lexicon, each)?,
            Some(Inside::Text { run }) if run < end => each(Token::Run { start: run, end })?,
            Some(Inside::Within) => continue,
            Some(Inside::Text { .. }) | None => {}
        }
        each(Token::Leave(cursor.node()))?;
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
    /// Whether a backslash right before a line break is blank, as C's and
    /// Python's line continuation is, in a gap and in a node hashed by its
    /// words.
    pub(super) continuations: bool,
}

impl Lexicon {
    /// The bytes of the line continuation `rest` opens with; 0 when it
    /// opens with none or the lexicon has none.
    pub(super) fn continuation(&self, rest: &str) -> usize {
        match (self.continuations, rest.as_bytes()) {
            (true, [b'\\', b'\n', ..]) => 2,
            (true, [b'\\', b'\r', b'\n', ..]) => 3,
            _ => 0,
        }
    }

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
/// nodes, that is neither whitespace, a line continuation nor a comment
/// of `lexicon`, as a token.
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
        let comment = lexicon.comment(rest).max(lexicon.continuation(rest));
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
            tokens(tree.root_node(), &text, &LEXICON, |token| {
                let (from, to) = match token {
                    Token::Node(node) => (node.start_byte(), node.end_byte()),
                    Token::Gap { start, end } | Token::Run { start, end } => (start, end),
                    Token::Hole { .. } | Token::Enter(_) | Token::Leave(_) => (0, 0),
                };
                for byte in &mut covered[from..to] {
                    assert!(!*byte, "{file}: byte {from} in two tokens");
                    *byte = true;
                }
                ControlFlow::Continue(())
            });
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
    fn a_parse_is_held_to_64_bytes_served_a_byte_of_its_file() {
        // Each line continuation of a run of 200 makes the scanner read to
        // the run's end, past the 64-byte chunk it holds, so the bytes
        // served are counted. A comment read once before them dilutes
        // that: one byte shorter it is past the bound, and with that byte
        // it is within.
        let python = tree_sitter_python::LANGUAGE.into();
        let hostile = format!("{}x\n", "\\\n".repeat(200)).repeat(40);
        let text = |comment: usize| format!("# {}\n{hostile}", "p".repeat(comment));

        assert!(parse(&text(12_743), &python).is_ok());
        assert_eq!(
            parse(&text(12_742), &python).err(),
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
