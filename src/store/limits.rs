//! The limits the store holds every file and value to, in one place, so a
//! surface that checks one cannot drift from the store.

/// Bytes of one node file or `project.toml`. A larger file is refused
/// before it is parsed: a node holds a decision, not a document, and the
/// cap bounds what one hostile file can cost a load. No write stages a
/// file over it (`Store::prepare_write`), and one record at every limit
/// below stays under it, its text and the accepted text a revision keeps
/// beside it included (the test below).
pub(super) const MAX_NODE_FILE_BYTES: usize = 1024 * 1024;

/// Maximum byte length for a decision's `reason` and a component's
/// `description`.
pub(crate) const MAX_TEXT_FIELD_BYTES: usize = 50_000;

/// Maximum byte length of one alternative, one line: a rejected option and
/// why, a sentence or two. A hundred of them, written with every byte
/// escaped, and kept twice in a revision's accepted text, are 400 KB.
pub(crate) const MAX_ALTERNATIVE_BYTES: usize = 1_000;

/// Maximum byte length of one tag, one line.
pub(crate) const MAX_TAG_BYTES: usize = 128;

/// Minimum byte length for a decision's `reason` field.
/// Forces actual reasoning instead of rubber-stamp approvals.
pub(super) const MIN_REASON_BYTES: usize = 10;

/// Maximum byte length for a decision's `choice` field.
/// A choice is a concise title, not a paragraph.
pub(crate) const MAX_CHOICE_BYTES: usize = 200;

/// Maximum byte length of an anchor's file path.
pub(crate) const MAX_ANCHOR_PATH_BYTES: usize = 500;

/// Maximum byte length of an anchor's symbol.
pub(crate) const MAX_ANCHOR_SYMBOL_BYTES: usize = 200;

/// Maximum byte length of the `Origin:` or the `Session:` a history commit
/// carries, each one line.
pub(crate) const MAX_TRAILER_BYTES: usize = 200;

/// Maximum byte length of a node's name, its file stem: within the 255
/// bytes most file systems allow a file name, `.toml` included, so no write
/// stages a file it cannot rename into place.
pub(crate) const MAX_NAME_BYTES: usize = 128;

/// Maximum ops in one `apply` batch: a result names each op, and fifty of
/// them fit one tool result whatever their names.
pub(crate) const MAX_BATCH_OPS: usize = 50;

/// Maximum items of one list an op sets, adds or removes.
pub(crate) const MAX_LIST_ITEMS: usize = 100;

/// Maximum `[quality] patterns` of one project: each is compiled for every
/// write, so the cap bounds what a hostile `project.toml` costs one.
pub(crate) const MAX_QUALITY_PATTERNS: usize = 32;

/// Maximum byte length of one `[quality]` pattern's text.
pub(super) const MAX_PATTERN_BYTES: usize = 500;

/// Maximum bytes one compiled pattern and its lazy DFA's cache may take;
/// a pattern that compiles past it is refused. A short text can still ask
/// for a large automaton: `\w{50}` over Unicode needs about 2.5 MB.
pub(super) const MAX_PATTERN_COMPILED_BYTES: usize = 1024 * 1024;

/// Maximum byte length of a `[quality]` pattern's message, one line.
pub(crate) const MAX_PATTERN_MESSAGE_BYTES: usize = 200;

/// Bytes of a pattern's match, or of an element a refusal names, that a
/// refusal quotes; a longer one is cut at a character boundary.
pub(crate) const MAX_QUOTED_MATCH_BYTES: usize = 40;

/// Warnings an `apply` result lists per op, and for the batch's `origin`;
/// the rest are counted. Each `warn` pattern warns once per op, so an op
/// can draw [`MAX_QUALITY_PATTERNS`] warnings and fifty ops 1,600: no
/// result lists them within its budget beside the ops it names. The
/// arithmetic is in `mcp/apply.rs`, beside the test that holds it.
pub(crate) const MAX_WARNINGS_PER_OP: usize = 1;

/// Bytes the messages of a project's `warn` patterns take together, as a
/// JSON string writes them (a `"` or a `\` counted twice): an `apply`
/// result carries each message a warning names once, and this bound keeps
/// the result of a full batch within its budget.
pub(crate) const MAX_WARN_MESSAGES_BYTES: usize = 1_536;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::canonical::canonical;
    use crate::store::schema::{AcceptedText, Anchor, Status};
    use crate::store::testing::sample_decision;

    /// One record at every limit of the `apply` spec, each text full of
    /// the characters the canonical form escapes, and the accepted text a
    /// revision keeps beside it: about 790 KB, under the cap a load reads.
    /// Only growth across ops (`add_*`, a merge's union) can pass the cap,
    /// which `Store::prepare_write` refuses.
    #[test]
    fn the_largest_decision_one_record_and_one_revision_write_fits_a_node_file() {
        let text = |bytes: usize| "\"".repeat(bytes);
        let name = |n: usize| format!("{n:0>width$}", width = MAX_NAME_BYTES);
        let mut decision = sample_decision(&name(0));
        decision.choice = text(MAX_CHOICE_BYTES);
        decision.reason = text(MAX_TEXT_FIELD_BYTES);
        decision.alternatives = vec![text(MAX_ALTERNATIVE_BYTES); MAX_LIST_ITEMS];
        decision.tags = (0..MAX_LIST_ITEMS)
            .map(|n| format!("{n:\"<width$}", width = MAX_TAG_BYTES))
            .collect();
        decision.anchors = (0..MAX_LIST_ITEMS)
            .map(|n| {
                let file = format!("{n:\"<width$}", width = MAX_ANCHOR_PATH_BYTES);
                Anchor::new(file, Some(text(MAX_ANCHOR_SYMBOL_BYTES))).unwrap()
            })
            .collect();
        decision.depends_on = (0..MAX_LIST_ITEMS).map(name).collect();
        decision.origin = Some(text(MAX_TRAILER_BYTES));
        decision.status = Status::Proposed {
            accepted_text: Some(AcceptedText {
                choice: decision.choice.clone(),
                reason: decision.reason.clone(),
                alternatives: decision.alternatives.clone(),
                via: None,
            }),
        };

        let bytes = canonical(&decision).len();

        assert!(bytes <= MAX_NODE_FILE_BYTES, "{bytes} bytes");
        assert!(bytes > MAX_NODE_FILE_BYTES / 2, "{bytes} bytes");
    }
}
