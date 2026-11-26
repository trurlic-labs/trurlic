## Trurlic

Architecture layer for AI-assisted codebases: a typed decision graph in `.trurlic/` (one TOML file per node), served to coding agents over MCP. No LLM calls.

The detail of every rule is in the decision graph (`.trurlic/decisions/`, one component per module) and in the module docs. This file is the map and the rules no tool enforces.

### Modules

Single crate (`src/lib.rs`). `pub` only on `cli::main`, `FORMAT_VERSION` and, with the `bench` feature, `bench`; everything else `pub(crate)`.

```
store      → git                         graph files: schema, canonical form, validation, text and
                                         quality rules, apply, commit/journal/recovery, lock,
                                         migrations, a shared graph's history
tools      → store, lint, sources,       the tools, known to no surface: specs (schema = parser),
             budget, discovery           `apply`, `status`, the pre-spec reads, registry, result cap
mcp        → tools, store                JSON-RPC stdio server: protocol, client, envelope, `tools/list`
commands   → store, tools, mcp (serve),  init, apply, status, query, check, fmt, migrate, install, serve
             sources, lint, git,
             discovery
cli        → commands, store             clap definitions and dispatch
discovery  → store, git                  graph and code root from the calling directory
lint       → store, code                 pure findings over the graph that refuse nothing: likely
                                         duplicate decisions, the bar a stored decision breaks,
                                         decisions with file anchors only, anchor findings and
                                         their repairs
sources    → code, lint, store, git      code-root walk, bounded reads, index cache, other worktrees,
                                         the anchor findings of `check` and `status`
code       → store (types)               pure code index: definitions, spans, span hashes, symbols
git        → —                           `git` as a child process; `git::graph` = the graph's own repo
budget     → —                           fitting output into a byte budget
error      → store (types)               `Error`, `Result`
console    → —                           `out!` (stdout), `diag!` (stderr)
```

`error` is used by every module but `code`, `lint` and `budget`; `console` by `store`, `tools`, `mcp`, `commands`, `cli` and `sources`.

### Where things are

| Topic | Home |
|---|---|
| Node types, `FORMAT_VERSION` (0.5.0) | `store/model/schema.rs` |
| Text rules / write-only quality bar | `store/model/text.rs` / `store/quality/` |
| Canonical serializer (the only one) | `store/format/canonical.rs` |
| The one write: ops, cascades, undo | `store/apply/` |
| Commit, journal, recovery | `store/write/commit.rs`, `journal.rs`, `recovery.rs` |
| Shared-graph history | `store/write/history.rs`, `git/graph/` |
| Format check / migration registry / carrying one out | `store/format/check.rs` / `store/format/migration/` / `store/write/migrate.rs` |
| Size and count limits | `store/model/limits.rs` |
| Tool specs (schema = parser) | `tools/spec/` |
| Refusal JSON, result fitting | `tools/result.rs`, `tools/truncate.rs` |
| Tool registry / its MCP listing and result envelope | `tools/registry.rs` / `mcp/listing.rs`, `mcp/envelope.rs` |
| Pre-spec reads (`get_context`, `get_decisions_for_file`) | `tools/legacy/` |
| Placement vs git (`check`) | `commands/placement.rs` |
| Index work budgets / cache encoding | `code/index/work.rs` / `code/index/encoding/` |
| Rust tokens no span hash holds (rustfmt's commas and braces) | `code/parse/rust/silent.rs` |
| The threads files are indexed on (pool, stack, bytes at once) | `sources/indexer.rs` |
| The tree-sitter runtime and the patch that bounds a parse | `vendor/tree-sitter/`, `vendor/tree-sitter-0.27.2-bounds.patch` |
| Likely duplicates (scoring, pair index) / which `status` call compares them | `lint/duplicates/` / `tools/status/lint/pairs.rs` |
| Anchor findings: kinds and verdicts / gathering / `status` section | `lint/anchors.rs` / `sources/findings.rs`, `sources/worktrees.rs` / `tools/status/anchors.rs` |
| Anchor repairs: rules / git's renames and history / the op / `apply`'s read | `lint/anchors/repair.rs` / `sources/repair/`, `git/renames.rs` / `store/apply/repair.rs` / `tools/apply/repairs.rs` |
| Quality findings, the `status` lint section | `lint/quality.rs`, `store/quality/bar/audit.rs`, `tools/status/lint/mod.rs` |
| A `[quality]` pattern's engine, its caches, the bytes it charges / what compiling it folds | `store/quality/bar/pattern.rs` / `store/quality/bar/folding.rs` |
| A language beyond Rust: grammar, query, test places, pre-parse bounds | `code/parse/language/mod.rs`, `code/parse/language/<name>.scm`, `code/parse/language/<name>.rs` |
| C and C++ macros blanked before the parse: the per-file pass / the sets / the headers gathered and the passes cached | `code/parse/language/c_family/mod.rs` / `c_family/set.rs` / `sources/reads.rs`, `sources/lexical.rs` |
| Span-hash units and hashing | `code/parse/units/` |
| Query contract / general resolver | `code/parse/query.rs` / `code/parse/general/` |
| Failpoint sites | `store/write/failpoint.rs` |

### Invariants

1. `unsafe_code = "forbid"`.
2. Lints are the rules (`Cargo.toml [lints]`, `clippy.toml`; `tests/lints.rs` seeds a violation of each): no `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` outside tests; no `HashMap`/`HashSet`; stdout/stderr only via `console`; clock only via `Timestamp::now`. Exceptions: `#[expect(lint, reason = "...")]`; `#[allow]` is refused.
3. Every graph write goes through a `Writer` (`Store::begin_write`), which owns the exclusive lock and the graph loaded under it. `init` and `migrate` take the lock themselves. `mcp` never writes directly.
4. A write adds no validation issue; a batch is one commit or nothing.
5. Commit order: canonical text read back → staged temp files → (shared) blobs → journal = commit point → renames → (shared) history commit → journal deleted. Recovery rolls forward under the exclusive lock; after the commit point a failure is `WritePending`, never a rollback.
6. `shared`: one commit per write in `.trurlic/.git`, after one `hand edit` commit, fixed identity, no user/system git config. `tracked`: trurlic never commits.
7. No state between calls: each call discovers and loads the store from disk.
8. Node files: read as their types (`deny_unknown_fields`, 1 MiB cap), written only in canonical form. Types derive `Deserialize`, never `Serialize`.
9. A load fails only on an unreadable file; everything else is a validation issue (`check` fails, reads still answer).
10. Every tool result fits 24 KiB (`budget::MAX_TOOL_RESULT_BYTES`) as valid JSON; cuts are named in `truncated`. Every refusal is an `isError` result `{"error": {"kind", ...}}`; JSON-RPC errors only for malformed messages, unknown methods/tools, panics.
11. A panic in a request is answered `-32603` and the server keeps serving (`panic = "unwind"`, one `catch_unwind` in `mcp::Server::answer`).
12. `code` is pure: its non-test code names only what an allow list holds (`code/purity.rs`, `code::tests`). Its cache (`.state/symbols/`) is never authoritative; a bad entry is a miss.
13. A change to what an index holds raises `code::INDEX_FORMAT` (the fingerprint test says when).
14. The store asks git nothing about the project's repository; `commands` gathers those facts and passes them as data.
15. Every dependency justified; no runtime proc macros.
16. A `[quality]` pattern compiles within `MAX_PATTERN_FOLDED_CODE_POINTS` and `MAX_PATTERN_COMPILED_BYTES`, before a write takes its lock, and runs on the lazy DFA alone, charging its bytes against `MAX_WRITE_PATTERN_BYTES` or `MAX_REPORT_PATTERN_BYTES` before it reads them, its cache giving up rather than outgrow `MAX_PATTERN_CACHE_BYTES`, and the caches of a call growing against `MAX_PATTERN_CACHE_TOTAL_BYTES` together (`store/model/limits.rs`). Past any of them a write is refused and a report reads no further.
17. `vendor/tree-sitter/` is upstream's crate but for `vendor/tree-sitter-0.27.2-bounds.patch`: a change to the runtime is a change to that patch, regenerated, and raises `code::INDEX_FORMAT`.

### Working with the graph

```
1. read the decisions of the files you will edit: grep anchors in .trurlic/decisions/*.toml,
   or get_decisions_for_file(path) / get_context(component)
2. implement within them; touching a second module → read its decisions too
3. a choice no decision covers, or one made stale: one batch file per change, with an origin,
   then `trurlic apply <file>` (or the apply tool)
4. `trurlic check` clean before committing
```

- Bar (`[quality]` in `.trurlic/project.toml`): reason 20–300 words, ≤ 5 alternatives of ≤ 50 words, 1–10 anchors, ASCII, `origin` required.
- `apply` records `proposed`. A choice the user made in this conversation: edit its file to `status = "accepted"` (no `accepted_via`). A revision of accepted text is proposed again.
- Design tasks: discuss the choices with the user first; record each one agreed.
- After implementing: re-read the decisions, confirm none is violated.

### Testing

| Command | Runs |
|---|---|
| `make check` | fmt, clippy (default and all features), `-D warnings`; the pre-commit hook |
| `make test` | unit + integration, with `failpoints` |
| `make ci` | check, test, `cargo deny` |
| `make bench` | criterion over the seeded corpus; local only |

- Unit tests sit in the file they test; integration tests (`tests/integration/`) drive the built binary through `harness`.
- Goldens: `tests/integration/golden/`, rewritten with `TRURLIC_UPDATE_GOLDEN=1`.
- Failpoints: `TRURLIC_FAILPOINT=<site>:<n>` (`TRURLIC_FAILPOINT_PAUSE` to pause instead of abort).
- Format fixtures: `tests/fixtures/formats/<version>/`, each migrated, loaded and checked.
- Code fixtures: `tests/fixtures/code/`; manifests are named `Cargo.toml.fixture`.
- Prose fixture: `tests/fixtures/prose/`, fixed English decision text the `[quality]` caches are sized on.
- Seeded corpus (`store::corpus`, `store::corpus::seeded`) feeds property tests and benches.
- No test for coverage: each asserts a property someone could break.

### Skills

- `trurlic`: before any task.
- `rust`: before any implementation.
- `review`: after any implementation.
