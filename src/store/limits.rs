//! Shared input-validation constants for all trust boundaries.
//!
//! Every entry point (MCP server, map REST API, CLI) enforces the same
//! limits on untrusted input. Defining them in one place prevents
//! silent drift between adapters — a constant here changes everywhere,
//! and the compiler catches any adapter that still uses a local copy.

/// Maximum byte length for any single text field (choice, reason,
/// description, tag, name, etc.). Design conversations are typically
/// 10–50 KB total; a single argument should never approach that.
pub const MAX_TEXT_FIELD_BYTES: usize = 50_000;

/// Maximum number of elements in any array field (alternatives,
/// depends_on, constrains, tags, decisions, components).
pub const MAX_ARRAY_ITEMS: usize = 100;

/// Minimum byte length for a decision's `reason` field.
/// Forces actual reasoning instead of rubber-stamp approvals.
pub const MIN_REASON_BYTES: usize = 10;

/// Maximum byte length for a decision's `choice` field.
/// A choice is a concise title, not a paragraph.
pub const MAX_CHOICE_BYTES: usize = 200;

/// Minimum byte length for step evidence on gated (interactive) steps.
/// Catches empty strings and trivial rubber stamps without requiring
/// semantic analysis of user input.
pub const MIN_STEP_EVIDENCE_BYTES: usize = 20;

/// Maximum number of code references per decision.
pub const MAX_CODE_REFS: usize = 20;

/// Maximum byte length for a code reference file path.
pub const MAX_CODE_REF_PATH_BYTES: usize = 500;

/// Maximum byte length for a code reference symbol name.
pub const MAX_CODE_REF_SYMBOL_BYTES: usize = 200;

/// Maximum number of history entries retained per decision.
/// History is a ring buffer: once full, revising a decision drops the
/// oldest entry so a single long-lived decision cannot grow without bound.
/// Twenty revisions of one decision is already well beyond normal use.
pub const MAX_HISTORY_ENTRIES: usize = 20;
