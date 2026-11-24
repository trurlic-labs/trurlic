//! Parsing a file with a tree-sitter grammar, within a budget of work,
//! and the leaf tokens a span hash covers. A grammar's own walk of the
//! tree (`rust.rs`) stands on these two.
//!
//! A tree-sitter parse recovers from any input, and on hostile input its
//! error recovery can take many parser steps per byte, and its lexer can
//! scan the rest of the file again from each byte (an unclosed raw string
//! repeated). Both are counted: the parser reports every hundred steps,
//! and the lexer reads the text in chunks of [`CHUNK_BYTES`] through a
//! callback, so the bytes it scans are the bytes the callback serves. A
//! parse past [`MAX_WORK_PER_BYTE`] steps, or bytes served, per byte of
//! the file ([`MIN_WORK_BUDGET`] for a small one) is stopped and the file
//! has no index. The bounds count work, not time, so a file is refused on
//! every machine or on none.

use std::ops::ControlFlow;

use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use super::index::IndexFault;
use super::span::SpanHash;

/// Parser steps, and bytes the lexer scans, per byte of the file. Source
/// code a person wrote takes about one of each; the bound leaves a wide
/// margin for code mid-edit.
const MAX_WORK_PER_BYTE: usize = 64;

/// The work any file may take, however short.
const MIN_WORK_BUDGET: usize = 1 << 20;

/// The steps between two of the parser's progress reports.
const STEPS_PER_REPORT: usize = 100;

/// The bytes the lexer is handed at a time.
const CHUNK_BYTES: usize = 1024;

/// The tree of `text` under `language`, or [`IndexFault::TooComplex`] when
/// the parse runs past its budget.
pub(super) fn parse(text: &str, language: &Language) -> Result<Tree, IndexFault> {
    let budget = text
        .len()
        .saturating_mul(MAX_WORK_PER_BYTE)
        .max(MIN_WORK_BUDGET);
    parse_within(text, language, budget)
}

/// The tree of `text`, within `budget` parser steps and bytes scanned.
fn parse_within(text: &str, language: &Language, budget: usize) -> Result<Tree, IndexFault> {
    let mut parser = Parser::new();
    // The one refusal is a grammar of an ABI this tree-sitter does not
    // read, and the grammars are pinned crates: it fails in a test or never.
    if parser.set_language(language).is_err() {
        return Err(IndexFault::Grammar);
    }
    let mut reports = 0_usize;
    let mut progress = |_: &_| {
        reports += 1;
        match reports > budget / STEPS_PER_REPORT {
            true => ControlFlow::Break(()),
            false => ControlFlow::Continue(()),
        }
    };
    let bytes = text.as_bytes();
    let mut served = 0_usize;
    // Past the budget the lexer is told the file has ended, so the parse
    // ends at once; the tree it gives is then discarded.
    let mut read = |offset: usize, _| {
        let rest = bytes.get(offset..).unwrap_or_default();
        let chunk = rest.get(..CHUNK_BYTES).unwrap_or(rest);
        served = served.saturating_add(chunk.len());
        if served > budget { &[] } else { chunk }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(&mut read, None, Some(options));
    match tree {
        Some(tree) if served <= budget => Ok(tree),
        Some(_) | None => Err(IndexFault::TooComplex),
    }
}

/// The byte ranges of a tree's tokens, in order: its leaves, and each
/// literal as one token over its whole range, so a string's prefix and
/// delimiters, which a grammar may hide (`br"x"`, `r#"x"#`), are hashed
/// with its content. Comments and every other extra are left out, and
/// tokens of no width (the parser's inserted missing tokens) too.
pub(super) struct Leaves {
    ranges: Vec<(usize, usize)>,
}

impl Leaves {
    pub(super) fn of(tree: &Tree) -> Self {
        let mut ranges = Vec::new();
        let mut cursor = tree.walk();
        'walk: loop {
            let node = cursor.node();
            let unit = node.child_count() == 0 || node.kind().ends_with("_literal");
            if !node.is_extra() && !unit && cursor.goto_first_child() {
                continue;
            }
            if !node.is_extra() && unit && node.end_byte() > node.start_byte() {
                ranges.push((node.start_byte(), node.end_byte()));
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
        Self { ranges }
    }

    /// The hash of the tokens within bytes `start..end` of `text`, and how
    /// many it covers.
    pub(super) fn hash(&self, text: &str, start: usize, end: usize) -> (SpanHash, usize) {
        let first = self.ranges.partition_point(|&(from, _)| from < start);
        let within = self.ranges.get(first..).unwrap_or_default();
        let count = within.partition_point(|&(_, to)| to <= end);
        let tokens = within
            .get(..count)
            .unwrap_or_default()
            .iter()
            .map(|&(from, to)| text.as_bytes().get(from..to).unwrap_or_default());
        (SpanHash::of_tokens(tokens), count)
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

    fn rust_tree(text: &str) -> Tree {
        parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap()
    }

    #[test]
    fn a_hash_of_leaves_ignores_comments_and_layout() {
        let one = "fn f(a: u8) -> u8 { a + 1 }";
        let two = "/// Doc.\nfn f(\n    a: u8 // why\n) -> u8 {\n    a /* x */ + 1\n}";
        let hash = |text: &str| Leaves::of(&rust_tree(text)).hash(text, 0, text.len()).0;

        assert_eq!(hash(one), hash(two));
        assert_ne!(hash(one), hash("fn f(a: u8) -> u8 { a + 2 }"));
        assert_ne!(hash("fn f() { \"a b\" }"), hash("fn f() { \"a  b\" }"));
    }

    #[test]
    fn a_literal_is_hashed_with_its_prefix_and_delimiters() {
        let hash = |text: &str| Leaves::of(&rust_tree(text)).hash(text, 0, text.len()).0;
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
    fn every_byte_but_comments_and_whitespace_is_in_one_token() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for file in [
            "src/code/rust.rs",
            "tests/fixtures/code/crates/ledger/src/lib.rs",
        ] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let tree = rust_tree(&text);
            let mut covered = vec![false; text.len()];
            for &(from, to) in &Leaves::of(&tree).ranges {
                for byte in &mut covered[from..to] {
                    assert!(!*byte, "{file}: byte {from} in two tokens");
                    *byte = true;
                }
            }
            let mut comments = vec![false; text.len()];
            let mut cursor = tree.walk();
            'walk: loop {
                let node = cursor.node();
                if node.is_extra() {
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
            for (at, byte) in text.bytes().enumerate() {
                let silent = byte.is_ascii_whitespace() || comments[at];
                assert!(
                    covered[at] || silent,
                    "{file}: byte {at} ({:?}) is in no token",
                    byte as char
                );
            }
        }
    }

    #[test]
    fn a_hash_covers_only_the_tokens_inside_its_span() {
        let text = "fn a() {}\nfn b() {}\n";
        let leaves = Leaves::of(&rust_tree(text));

        assert_eq!(
            leaves.hash(text, 0, 9),
            Leaves::of(&rust_tree("fn a() {}")).hash("fn a() {}", 0, 9)
        );
        assert_ne!(leaves.hash(text, 0, 9).0, leaves.hash(text, 10, 19).0);
    }

    #[test]
    fn a_parse_past_its_budget_gives_no_tree() {
        let rust = tree_sitter_rust::LANGUAGE.into();
        let text = "fn f() {}\n".repeat(1_000);

        assert!(parse_within(&text, &rust, text.len() * 4).is_ok());
        assert_eq!(
            parse_within(&text, &rust, text.len() / 2).err(),
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
