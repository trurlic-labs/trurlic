## Trurlic

Architecture layer for AI-assisted codebases: a typed decision graph in `.trurlic/` (one TOML file per node), served to coding agents over MCP. No LLM calls.

The detail of every rule is in the decision graph (`.trurlic/decisions/`, one component per module) and in the module docs. This file is the map and the rules no tool enforces.

### Modules

Single crate (`src/lib.rs`). `pub` only on `cli` and `store`; everything else `pub(crate)`.

```
store      → git                         graph files: schema, canonical form, validation, text and
                                         quality rules, apply, commit/journal/recovery, lock,
                                         migrations, a shared graph's history
mcp        → store, lint, sources,       JSON-RPC stdio server, tool specs, `apply` and `status`
             budget, discovery           and their CLI twins, context reads, result cap
commands   → store, mcp, sources, lint,  init, apply, status, query, check, fmt, migrate, install, serve
             git, discovery
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

`error` and `console` are used by every module but `code` and `lint`.

### Where things are

| Topic | Home |
|---|---|
| Node types, `FORMAT_VERSION` (0.5.0) | `store/schema.rs` |
| Text rules / write-only quality bar | `store/text.rs` / `store/quality/` |
| Canonical serializer (the only one) | `store/canonical.rs` |
| The one write: ops, cascades, undo | `store/apply/` |
| Commit, journal, recovery | `store/commit.rs`, `journal.rs`, `recovery.rs` |
| Shared-graph history | `store/history.rs`, `git/graph/` |
| Format check / migration registry | `store/version.rs` / `store/migration/` |
| Size and count limits | `store/limits.rs` |
| Tool specs (schema = parser) | `mcp/spec/` |
| Refusal JSON, result fitting | `mcp/result.rs`, `mcp/truncate.rs` |
| Pre-spec reads (`get_context`, `get_decisions_for_file`) | `mcp/tools.rs`, `mcp/context.rs` |
| Placement vs git (`check`) | `commands/placement.rs` |
| Index work budgets / cache encoding | `code/work.rs` / `code/encoding/` |
| Likely duplicates (scoring, pair index) | `lint/duplicates/` |
| Anchor findings: kinds and verdicts / gathering / `status` section | `lint/anchors.rs` / `sources/mod.rs`, `sources/worktrees.rs` / `mcp/status/anchors.rs` |
| Anchor repairs: rules / git's renames and history / the op / `apply`'s read | `lint/anchors/repair.rs` / `sources/renames.rs`, `git/renames.rs` / `store/apply/repair.rs` / `mcp/apply/repairs.rs` |
| Quality findings, the `status` lint section | `lint/quality.rs`, `store/quality/bar/audit.rs`, `mcp/status/lint.rs` |
| Work charged before `[quality]` patterns run | `store/quality/bar/work.rs`, `store/apply/patterns.rs` |
| A language beyond Rust: grammar, query, test places | `code/language.rs`, `code/language/<name>.scm` |
| Query contract / general resolver | `code/query.rs` / `code/general.rs` |
| Failpoint sites | `store/failpoint.rs` |

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
16. No `[quality]` pattern runs before its work over the call is charged against `MAX_PATTERN_WORK` (`store/limits.rs`). Past it a write is refused and a report reads no further.

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
- Seeded corpus (`store::corpus`, `store::seeded`) feeds property tests and benches.
- No test for coverage: each asserts a property someone could break.

### Skills

- `trurlic`: before any task.
- `rust`: before any implementation.
- `review`: after any implementation.
