//! Parsing a file with a tree-sitter grammar, within two budgets of work.
//! Every tree-sitter resolver (`rust/`, `general/`) parses through it,
//! and walks the tree by its tokens (`tokens.rs`).
//!
//! A tree-sitter parse recovers from any input, at a cost the input
//! chooses. Its lexer can scan the rest of the file again from each byte
//! (an unclosed raw string repeated); it reads the text in chunks of
//! [`CHUNK_BYTES`] through a callback, so the bytes it scans are the bytes
//! the callback serves, and a parse past [`SERVED_BYTES_PER_BYTE`] of them
//! per byte is stopped. Its error recovery and its ambiguities can take
//! work quadratic in the file, and memory with it; the runtime
//! (`vendor/tree-sitter`) counts each parser action, each step of a walk
//! of its stack and each 64 bytes it allocates as one progress operation,
//! and a parse past [`PARSE_OPERATIONS_PER_BYTE`] of them per byte is
//! stopped at the runtime's next check of the count, which comes between
//! two of its steps, never inside one. A stopped parse leaves the file
//! with no index. Both bounds count work, not time, so a file is refused
//! on every machine or on none. The runtime releases and merges its stack
//! without recursion, so a parse takes the same call stack whatever its
//! file.

use std::ops::ControlFlow;

use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use crate::code::index::{
    IndexFault, MIN_PARSE_OPERATIONS, MIN_WORK, PARSE_OPERATIONS_PER_BYTE, SERVED_BYTES_PER_BYTE,
    Work,
};

/// The bytes the lexer is handed at a time. The lexer reads again within
/// the chunk it holds without asking for it, so a rescan inside one chunk
/// is uncounted: a small chunk keeps that uncounted rescan short.
pub(in crate::code) const CHUNK_BYTES: usize = 64;

/// The progress operations between two calls of the progress callback,
/// the runtime's own constant.
pub(in crate::code) const OPERATIONS_PER_PROGRESS: usize = 100;

/// The tree of `text` under `language`, or [`IndexFault::TooComplex`] when
/// the parse runs past either of its budgets.
pub(crate) fn parse(text: &str, language: &Language) -> Result<Tree, IndexFault> {
    // A parser per parse: a parser that parsed before reuses its pools and
    // counts fewer operations, so only a fresh one makes the verdict a
    // function of the file.
    let mut parser = Parser::new();
    // The one refusal is a grammar of an ABI this tree-sitter does not
    // read, and the grammars are pinned crates: it fails in a test or never.
    if parser.set_language(language).is_err() {
        return Err(IndexFault::Grammar);
    }
    let bytes = text.as_bytes();
    let mut served = Work::of(text.len(), SERVED_BYTES_PER_BYTE, MIN_WORK);
    let mut within = true;
    // Past the budget the lexer is told the file has ended, so the parse
    // ends at once; the tree it gives is then discarded.
    let mut read = |offset: usize, _| {
        let rest = bytes.get(offset..).unwrap_or_default();
        let chunk = rest.get(..CHUNK_BYTES).unwrap_or(rest);
        within &= served.spend(chunk.len()).is_ok();
        if within { chunk } else { &[] }
    };
    let mut operations = Work::of(text.len(), PARSE_OPERATIONS_PER_BYTE, MIN_PARSE_OPERATIONS);
    // Past the budget the parse is cancelled and gives no tree.
    let mut progress = |_: &_| match operations.spend(OPERATIONS_PER_PROGRESS) {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(()),
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(&mut read, None, Some(options));
    match tree {
        Some(tree) if within => Ok(tree),
        Some(_) | None => Err(IndexFault::TooComplex),
    }
}

/// The identity of a grammar: its ABI, its version, the counts of its
/// symbols, fields and parse states, and its node types, hashed.
pub(in crate::code) fn grammar_identity(language: &Language, node_types: &str) -> [u8; 32] {
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
pub(crate) fn text_of<'t>(node: Node<'_>, text: &'t str) -> &'t str {
    text.get(node.byte_range()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `unit` repeated after `open` to 16 KiB.
    fn repeated(open: &str, unit: &str) -> String {
        format!(
            "{open}{}",
            unit.repeat((16 * 1024 - open.len()) / unit.len())
        )
    }

    #[test]
    fn error_recovery_past_its_operations_a_byte_is_refused_in_every_grammar() {
        // Each unit is an error the next cannot recover from, so recovery
        // walks the whole stack again at each: work quadratic in the file,
        // which each grammar refuses.
        let cases: [(&str, Language, String); 6] = [
            (
                "python",
                tree_sitter_python::LANGUAGE.into(),
                repeated("", " if ~"),
            ),
            (
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                repeated("x=", "|-"),
            ),
            (
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                repeated("x=", "|-"),
            ),
            (
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                repeated("fn f() { ", " ! ]"),
            ),
            (
                "c",
                tree_sitter_c::LANGUAGE.into(),
                repeated("void f() { ", " & }"),
            ),
            (
                "cpp",
                tree_sitter_cpp::LANGUAGE.into(),
                repeated("void f() { ", " - )"),
            ),
        ];
        for (grammar, language, text) in cases {
            assert_eq!(
                parse(&text, &language).err(),
                Some(IndexFault::TooComplex),
                "{grammar}"
            );
        }
    }

    #[test]
    fn a_parse_whose_stack_walks_pass_the_budget_is_refused() {
        // A run of line continuations in an error: recovery walks the run
        // at each one. Its actions and allocation take less than half the
        // budget, so it is refused only as each step of a walk is counted.
        let hostile = repeated("", "\\\nx");

        assert_eq!(
            parse(&hostile, &tree_sitter_python::LANGUAGE.into()).err(),
            Some(IndexFault::TooComplex)
        );
    }

    #[test]
    fn a_parse_whose_allocation_passes_the_budget_is_refused() {
        // Unclosed template arguments keep ambiguous readings alive, which
        // copy the stack as they fork. Its actions and walks take nearly
        // all of the budget and not more, so it is refused only as every
        // 64 bytes allocated are counted.
        let hostile = repeated("", "a<b>(");

        assert_eq!(
            parse(&hostile, &tree_sitter_cpp::LANGUAGE.into()).err(),
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

    #[test]
    fn a_lexer_that_rescans_within_its_chunk_is_stopped_within_its_budget() {
        // Each line continuation makes the scanner read ahead within the
        // chunk the lexer holds; at 1,024 bytes a chunk this went uncounted.
        let hostile = format!("{}x\n", "\\\n".repeat(500)).repeat(1_000);

        assert_eq!(
            parse(&hostile, &tree_sitter_python::LANGUAGE.into()).err(),
            Some(IndexFault::TooComplex)
        );
    }
}
