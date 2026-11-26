//! The limits the store holds every file and value to, in one place, so a
//! surface that checks one cannot drift from the store.

/// Bytes of one node file or `project.toml`. A larger file is refused
/// before it is parsed: a node holds a decision, not a document, and the
/// cap bounds what one hostile file can cost a load. No write stages a
/// file over it (`Store::prepare_write`), and one record at every limit
/// below stays under it, its text and the accepted text a revision keeps
/// beside it included (the test below).
pub(in crate::store) const MAX_NODE_FILE_BYTES: usize = 1024 * 1024;

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
pub(in crate::store) const MIN_REASON_BYTES: usize = 10;

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
pub(in crate::store) const MAX_PATTERN_BYTES: usize = 500;

/// Maximum bytes the Thompson NFA of one pattern, forward or reverse, may
/// take to compile; a pattern past it is refused. A short text can still
/// ask for a large automaton: `\w{100}` over Unicode passes it.
pub(in crate::store) const MAX_PATTERN_COMPILED_BYTES: usize = 1024 * 1024;

/// Code points the case-insensitive classes of one pattern may span:
/// translating a pattern folds each class it reads under the flag code
/// point by code point, and the sum is counted on its syntax tree before it
/// is translated, the flag scoped as translation scopes it. A pattern past
/// it is refused when compiled; a case-sensitive class, and a Perl class
/// anywhere, fold nothing. The bound admits one class of every code point
/// under the flag, `(?i)[\s\S]` (1,114,112), and nearly as much again
/// beside it, and refuses what nested brackets fold many times over. A
/// table of 32 patterns at the bound compiles in about 0.15 s on one core.
pub(in crate::store) const MAX_PATTERN_FOLDED_CODE_POINTS: u64 = 1 << 21;

/// Bytes of cache the lazy DFA of one pattern may hold in each direction
/// over one call. A pattern whose automaton cannot start within it is
/// refused when compiled (`\w{9}`); a search that would outgrow it gives up
/// instead of clearing it. The costliest of 32 ordinary patterns, a list
/// of 48 words under `(?i)`, grows its caches by about 224 KB over the 213
/// decisions of English prose in `tests/fixtures/prose`, and by about 20 KB
/// more each time the text doubles.
pub(in crate::store) const MAX_PATTERN_CACHE_BYTES: usize = 320 * 1024;

/// Bytes the caches of all the `[quality]` patterns, both directions, may
/// grow by over one call: one `apply` batch, or one page of the `status`
/// lint section. Growth is what building states costs, which the bytes
/// read do not count. It is read from a pattern's caches after its search
/// of each text, so it may pass the total by one search's growth, one
/// pattern's two caches; past it the write is refused at that text and the
/// page ends. What a search built stays charged, a repair the write leaves
/// included. It holds a table of 32 ordinary patterns over a full batch of
/// prose in many scripts.
pub(crate) const MAX_PATTERN_CACHE_TOTAL_BYTES: usize = 1536 * 1024;

/// Bytes the `[quality]` patterns may read over what one `apply` batch
/// writes: each pattern charges the bytes of each text it searches, one
/// more per search, before it runs. The search back from a match's end to
/// its start reads the match again, so the patterns read at most twice
/// what they charge. A write judges under the exclusive
/// lock, which another reader or writer waits for at most `LOCK_TIMEOUT`
/// (5 s), so the bound is the write's own and lower than a report's; with
/// [`MAX_PATTERN_CACHE_TOTAL_BYTES`] it bounds what the patterns cost a
/// write once compiled.
pub(crate) const MAX_WRITE_PATTERN_BYTES: usize = 100_000_000;

/// Bytes the `[quality]` patterns may read in one `status` lint section,
/// counted as a write counts them. The section holds no lock; it reads
/// the decisions by name while their bytes stay within the bound, and
/// names the first it left in `next`.
pub(crate) const MAX_REPORT_PATTERN_BYTES: usize = 2_500_000_000;

/// The work the likely duplicates pass may do in one `status` call, in
/// units of a posting entry read: each probe's entries and 16 for each
/// pair it may test (`lint::duplicates`), counted before it runs. The pass
/// grows with the square of the decisions, so only `section: "lint"`
/// without `from` runs it: the default call, which an agent makes at the
/// start of a task, compares no pair, as it judges no `[quality]` pattern.
/// The pass probes the decisions by name while their work fits, and past
/// the bound `counts.lint.pairs` says how many it compared. Decisions that
/// share all their terms, the costliest shape per unit, take about 2 ns a
/// unit on one core of the development machine: a pass at the bound costs
/// about 12 s of CPU, which the thread pool shares. The seeded corpus does
/// about 72 million units at 2,000 decisions.
pub(crate) const MAX_DUPLICATE_WORK: usize = 6_000_000_000;

/// Maximum byte length of a `[quality]` pattern's message, one line.
pub(crate) const MAX_PATTERN_MESSAGE_BYTES: usize = 200;

/// Bytes of a pattern's match, or of an element a refusal names, that a
/// refusal quotes; a longer one is cut at a character boundary.
pub(crate) const MAX_QUOTED_MATCH_BYTES: usize = 40;

/// Warnings an `apply` result lists per op, and for the batch's `origin`;
/// the rest are counted. Each `warn` pattern warns once per op, so an op
/// can draw [`MAX_QUALITY_PATTERNS`] warnings and fifty ops 1,600: no
/// result lists them within its budget beside the ops it names. The
/// arithmetic is in `tools/apply/result.rs`, beside the test that holds it.
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
    use crate::store::model::{AcceptedText, Anchor, Status};
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
