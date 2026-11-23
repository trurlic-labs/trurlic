## Trurlic

Architecture layer for AI-assisted codebases. Typed decision graph stored in `.trurlic/`, served to coding agents over MCP: the decisions and project-wide rules that constrain a component before code is written, agent-recorded decisions flagged for human review, and changed files checked against the decisions that cover them.

Named after Trurl (Stanisław Lem, *The Cyberiad*) — the constructor who thinks deeply about what he builds before building it.

### Architecture

Single crate, seven modules (`src/lib.rs`). Visibility enforces boundaries — `pub(crate)` on everything except `cli` and `store`.

```
store       → (no internal deps)         Decision graph: TOML files, graph index,
                                          validation, atomic writes, file locking
mcp         → store, budget               MCP server: JSON-RPC stdio, version
                                          negotiation, the server's own requests,
                                          argument specs, tool dispatch, context
                                          assembly, decision verification, the
                                          24 KiB cap on every tool result
commands    → store, mcp                  CLI command handlers: init, add, rename,
                                          remove, decide, query, status, check, gc,
                                          migrate, install (registers the MCP
                                          server with Claude Code), and the
                                          `serve` entry point
cli         → commands                    clap definitions and dispatch
budget      → (no internal deps)          Fitting output into a byte budget: the
                                          level search behind the result cap
error       → store (types)               The crate's `Error` enum and `Result`
                                          alias; it names the store's `Issue`,
                                          text-fault and timestamp-fault types
console     → (no internal deps)          Terminal I/O: `out!` to stdout, `diag!`
                                          to stderr, the confirmation prompt
```

Every module uses `error` and `console`; the arrows leave them out.

**store** is the foundation. It imports no other module except `error` and `console`. Every graph write is a method of the `Writer` that `Store::begin_write` returns, which owns the exclusive lock and the graph loaded under it; the commit path takes `&StoreLock` as proof of the lock. A decision's choice and reason are checked there (`store::text`), so every surface refuses the same text with the same message.

**error** carries the path of the file behind every I/O and TOML failure (`Error::Io`, `Error::Toml`); nothing converts a bare `io::Error` into it, so a `?` cannot drop the path. `commands::install` alone returns its own `InstallError`, which wraps `Error`; `cli` prints either as `error: <message>` with exit status 1.

**mcp** never writes to the graph directly. It calls `Writer` methods, and holds no graph between calls. A tool is built on an argument spec (`mcp::spec`): its `Args::declare` is walked once for the `inputSchema` and once per call for the parse, so the schema and the parser cannot drift, and an unknown key, a wrong type or a broken limit is refused naming the field. Its `Access` (read, write, interactive) decides the lock the call takes. Every refusal, a missing store included, is a tool result with `isError: true` whose text is `{"error": {"kind", "field"?, "message"}}` (`mcp::result`); a JSON-RPC error answers only a malformed message, an unknown method or tool, or a panic. The tools that predate specs keep their hand-written schemas in `mcp::tools` until the rows that replace them.

**mcp transport**: `initialize` answers the client's protocol revision when it is in `PROTOCOL_VERSIONS`, else the latest, and keeps the client's capabilities. During a tool call a handler can send the client a request through its `Peer` (`mcp::peer`) and wait for the answer on the same stdin, without a timeout: meanwhile a `ping` is answered, other requests are queued (16 at most) and served after the call, and `notifications/cancelled` for the call ends the wait with no response. Every line gets at most one answer; a line over 10 MiB is discarded whole.

**commands** reaches `mcp` only to start the MCP server (`serve`); every other handler reads and writes the graph through `Store`.

Trurlic makes no LLM calls. Design work happens in the agent that calls the MCP tools.

### Store Internals

Graph on disk: `.trurlic/` with `components/`, `decisions/`, `patterns/` subdirectories. Each node is a TOML file. `graph.toml` is a compiled edge index rebuilt deterministically from node files. `.trurlic/.state/` holds the lock file, temp files and the commit journal (`txn.toml`); it is never committed.

Commits: serialize and parse back in memory → stage each file in `.state/tmp/` under a per-process name and flush it → write and flush the journal, which names every staged file and its target: the commit point → rename the staged files into place, `graph.toml` last, remove deleted files, flush the directories → delete the journal. After the journal a failure returns `CommitPending`; the commit is not rolled back.

Recovery: a leftover journal is applied, then leftover temp files removed, only under the exclusive lock: by every write before it loads, and by every read that finds either one under the shared lock.

Timestamps: `store::Timestamp`, a UTC instant to the nanosecond, written as an RFC 3339 string in the form 0.3 releases wrote (`2025-06-01T10:30:00Z`, a fraction of 0, 3, 6 or 9 digits), so a load and a write give back the same bytes. `Timestamp::now` is the crate's one clock read.

Format version: `project.toml` states `format_version`, `MAJOR.MINOR.PATCH` and independent of the crate's version (`store::FormatVersion`, `FORMAT_VERSION` in `store/schema.rs`). The check has one home, `store/version.rs`: every read and every write refuses another format before it recovers anything or parses a node file. A newer one says to upgrade trurlic, an older one to run `trurlic migrate`, and a version that does not parse is named with its file and its text. `store::migration` holds the registry, one step per format change from 0.1.0 on; `trurlic migrate` walks it from the store's version, reports every value a step or the current format drops, and lands the result in one journaled commit whose first rename is `project.toml`. A migration stopped before that rename leaves an old-format store with a journal, which only the next `migrate` finishes; once it has happened, the store is current and the next command of any kind finishes it.

Content integrity: BLAKE3 hash per node file, stored in `graph.toml`. `trurlic check` verifies hashes. Tamper detection, not encryption.

File locking: std `File::try_lock` (writers, exclusive) and `File::try_lock_shared` (readers) on `.state/lock`, polled with a 5 s timeout. `StoreLock` is a proof-of-lock type: every function that changes a file under `.trurlic/` takes `&StoreLock`, and a `Writer` owns one.

Writer protocol: every write starts at `Store::begin_write`, which takes the exclusive file lock, checks the format, recovers, loads the graph from disk and returns a `Writer` that owns the lock and that graph. Every graph write is a `Writer` method, so all validation runs against the graph loaded under the lock, and each commit leaves the writer's graph equal to disk. Dropping the writer releases the lock.

Readers: every read (`Store::load_state`) takes the shared lock, checks the format and loads the graph. A journal or a temp file seen under the shared lock was left by a writer that stopped, so the read takes the exclusive lock, finishes that commit and loads under it: no read sees half of a commit.

In-memory state: `ProjectState` holds `BTreeMap`s of `Arc<ComponentFile>`, `Arc<DecisionFile>`, `Arc<PatternFile>`, plus the `GraphIndex` and an eagerly built `InMemoryGraph` for graph queries. It lives for one call.

No state between calls: the MCP server holds the directory it was started in, and every tool call finds the store from there and loads it. A read tool runs on `Store::load_state`, a write tool on a `Writer`, so a hand edit, a `git checkout` or another process's commit is seen by the very next call. `serve` starts without a store or with one in another format, and each call is refused until it is fixed, naming `trurlic init`, `trurlic migrate`, or the file and text of a version that does not parse. `load_state` reads the node files in parallel and parses `graph.toml` meanwhile; `make bench` times it.

### Key Invariants

1. `unsafe` is forbidden (`[lints.rust] unsafe_code = "forbid"` in Cargo.toml): no `#[expect]` can lift it.
2. Invariants a tool can check are lints, in Cargo.toml's `[lints]` and `clippy.toml`, and `tests/lints.rs` proves each refuses a seeded violation: no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` or `dbg!` outside tests; no `HashMap`/`HashSet`; stdout and stderr only through `console` (and the MCP transport, which owns stdout under `serve`); the clock only through `Timestamp::now`. An exception is an `#[expect(lint, reason = "...")]` at its site; `#[allow]` is refused.
3. Every graph mutation validates the full graph before touching disk. A write that adds an error is refused, never silently committed; an error the graph already had, matched on kind and subject, does not block it.
4. Atomic commits: round-trip in memory → flushed temp files → flushed journal (the commit point) → renames, `graph.toml` last → directory flushes. Recovery rolls a journal forward under the exclusive lock.
5. File locking prevents concurrent mutations from CLI and MCP processes.
6. What a call returns is a function of the files on disk: no process holds graph state between calls (`tests/integration/stdio.rs` sees a hand edit in the next call; `tests/integration/writers.rs` interleaves three servers' writes).
7. Boundary types (`DecisionFile`, `PatternFile`, `ComponentFile`, `GraphIndex`) derive `Serialize + Deserialize`. Internal types (`InMemoryGraph`) do not.
8. Every dependency justified. No proc macros at runtime (serde derive, thiserror are compile-time).
9. Every MCP tool result fits `budget::MAX_TOOL_RESULT_BYTES` (24 KiB) and stays valid JSON; a cut is named in its `truncated` array. Every tool description fits 2,048 characters (`mcp::tools` tests).
10. A panic while a request is handled is answered `-32603` and the server serves the next request: release builds unwind (`panic = "unwind"`), `mcp::Server::answer` is the one `catch_unwind`, and nothing outlives a request half-done (no graph is held; unwinding drops a writer's lock; every line is built before it is written). `tests/integration/server_failpoints.rs` panics under the exclusive lock and writes again.

### Trurlic

This project uses Trurlic for its own architectural decisions. Before a
task, read the decisions that constrain it; implement within them; record
the choices they leave open; verify before committing.

```
1. get_context(component) → brief with every decision and project rule
   the code must respect
2. implement within the brief
3. a choice no decision covers:
   check_pattern(description) → if uncovered:
     record_decision(component, choice, reason, alternatives, attribution)
4. verify_against_decisions(component, changed_files) → read each
   verdict, fix any VIOLATED decision before committing
```

- When touching a second module, call `get_context` for that module's component too.
- Attribution: `user` when the user made the choice in this conversation, `agent` when you made it alone; an agent decision is flagged ⚠ for human review until the user confirms it.
- When the task is to design something, discuss the choices with the user before implementing, and record each one they agree on.
- After implementation, re-read the brief and check no decision was silently violated.

### Testing

Unit tests: pure functions, same file. Every module has exhaustive tests for its public contract. Schema round-trips for every serializable type.

Integration tests (`tests/integration/`): drive the built binary. `harness` spawns `trurlic serve` and speaks JSON-RPC over stdio, playing the client for the server's own requests too; `golden` compares output with `tests/integration/golden/` (`TRURLIC_UPDATE_GOLDEN=1` rewrites the files). Every tool in `tools/list` is called once. `stdio.rs` covers each protocol case: version negotiation, each malformed line, an oversize line, an unknown tool, a server with no store or an old one.

Argument specs: `mcp::spec` holds one table-driven test over a spec with every field kind the 0.4.0 tools need, refusing each malformed shape with its field named.

Failpoints: the `failpoints` cargo feature (test job only) makes `TRURLIC_FAILPOINT=<site>:<n>` act at the n-th hit of a named site in `src/store/failpoint.rs`: abort (or pause, with `TRURLIC_FAILPOINT_PAUSE`) at a `hit`, return an injected I/O error at a `fail`, panic at a `panic` (`write.locked`), and let the caller act at a `triggers` site (`mcp.server_request`, where a tool call sends the client an elicitation form instead of running). `make test` enables it.

Property: determinism (every read tool answers with the same bytes in two processes), graph validation catches all known violation classes.

Format fixtures (`tests/fixtures/formats/<version>/`): a graph written by the trurlic of each historical format. Each one migrates to the current format, loads and passes `check` in a named test, its report and result pinned by a golden file.

No test for the sake of coverage. Every test asserts a property someone could break.

Benchmarks (`benches/corpus.rs`, criterion, run locally with `make bench`; no CI job): over the seeded corpus `store::corpus` at 50, 600 and 2000 decisions, `load_state`, graph build and validation, and, through `trurlic::bench::Server` in process, `record_decision` end to end and every read tool, each with the load every call makes. The `bench` feature builds the corpus and that server; release builds carry neither.

### Skills

- `rust` — Non-negotiable Rust code rules. Load before any implementation task.
- `review` — Post-implementation quality gate. Load after any implementation task.
- `trurlic` — How to use Trurlic's MCP tools. Load before any task.
