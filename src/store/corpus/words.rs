//! The word tables a corpus draws its text and names from.

const SYMBOLS_PER_FILE: usize = 8;

/// Prose vocabulary of architecture terms, so text reads like the text of
/// a production graph.
pub(super) const WORDS: &[&str] = &[
    "the",
    "store",
    "writes",
    "each",
    "node",
    "file",
    "under",
    "lock",
    "and",
    "reads",
    "it",
    "back",
    "before",
    "commit",
    "so",
    "a",
    "crash",
    "leaves",
    "graph",
    "valid",
    "token",
    "auth",
    "boundary",
    "error",
    "retry",
    "timeout",
    "cache",
    "latency",
    "budget",
    "hash",
    "validate",
    "integrity",
    "thread",
    "concurrent",
    "api",
    "protocol",
    "endpoint",
    "test",
    "fixture",
    "config",
    "default",
    "module",
    "layer",
    "dependency",
    "log",
    "metric",
    "trace",
    "schema",
    "format",
    "migration",
    "version",
    "name",
    "path",
    "request",
    "response",
    "client",
    "server",
    "queue",
    "batch",
    "index",
    "memory",
    "parse",
    "render",
    "refuse",
];

/// Component names; a corpus with more components than these suffixes them.
pub(super) const COMPONENT_WORDS: &[&str] = &[
    "auth",
    "billing",
    "catalog",
    "gateway",
    "ingest",
    "ledger",
    "mailer",
    "notify",
    "orders",
    "pricing",
    "reports",
    "search",
    "session",
    "storage",
    "telemetry",
    "workers",
];

/// Source file stems inside a component directory.
pub(super) const MODULE_WORDS: &[&str] = &[
    "api", "cache", "client", "config", "error", "handler", "index", "model", "parser", "queue",
    "render", "schema", "service", "store", "types", "worker",
];

/// Function name prefixes; one file holds one function per prefix.
pub(super) const SYMBOL_WORDS: [&str; SYMBOLS_PER_FILE] = [
    "build", "check", "load", "parse", "read", "render", "store", "write",
];

pub(super) const TAGS: &[&str] = &[
    "api",
    "concurrency",
    "error-handling",
    "integrity",
    "performance",
    "persistence",
    "security",
    "testing",
];
