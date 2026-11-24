//! The limits the store holds every file and value to, in one place, so a
//! surface that checks one cannot drift from the store.

/// Bytes of one node file or `project.toml`. A larger file is refused
/// before it is parsed: a node holds a decision, not a document, and the
/// cap bounds what one hostile file can cost a load.
pub(crate) const MAX_NODE_FILE_BYTES: usize = 1024 * 1024;

/// Maximum byte length for a decision's `reason`.
pub(crate) const MAX_TEXT_FIELD_BYTES: usize = 50_000;

/// Minimum byte length for a decision's `reason` field.
/// Forces actual reasoning instead of rubber-stamp approvals.
pub(crate) const MIN_REASON_BYTES: usize = 10;

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
