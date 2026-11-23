## Trurlic

Architecture layer for AI-assisted codebases. Typed decision graph stored in `.trurlic/`, served to coding agents over MCP: the decisions and project-wide rules that constrain a component before code is written, agent-recorded decisions flagged for human review, and changed files checked against the decisions that cover them.

Named after Trurl (Stanisław Lem, *The Cyberiad*) — the constructor who thinks deeply about what he builds before building it.

### Architecture

Single crate, seven modules (`src/lib.rs`). Visibility enforces boundaries — `pub(crate)` on everything except `cli` and `store`.

```
store       → (no internal deps)         Decision graph: TOML files, graph index,
                                          validation, atomic writes, file locking,
                                          file watcher
mcp         → store, budget               MCP server: JSON-RPC stdio, tool dispatch,
                                          context assembly, decision verification,
                                          the 24 KiB cap on every tool result
commands    → store, mcp                  CLI command handlers: init, add, rename,
                                          remove, decide, query, status, check, gc,
                                          migrate, install (registers the MCP
                                          server with Claude Code), and the
                                          `serve` entry point
cli         → commands                    clap definitions and dispatch
budget      → (no internal deps)          Fitting output into a byte budget: the
                                          level search behind the result cap
error       → store (types)               The crate's `Error` enum and `Result`
                                          alias; it names the store's `Issue`
                                          and text-fault types
console     → (no internal deps)          Terminal I/O: `out!` to stdout, `diag!`
                                          to stderr, the confirmation prompt
```

Every module uses `error` and `console`; the arrows leave them out.

**store** is the foundation. It imports no other module except `error` and `console`. Every write goes through `Store` methods with `StoreLock` proof parameters. A decision's choice and reason are checked there (`store::text`), so every surface refuses the same text with the same message.

**error** carries the path of the file behind every I/O and TOML failure (`Error::Io`, `Error::Toml`); nothing converts a bare `io::Error` into it, so a `?` cannot drop the path. `commands::install` alone returns its own `InstallError`, which wraps `Error`; `cli` prints either as `error: <message>` with exit status 1.

**mcp** never writes to the graph directly. It calls `Store` write methods.

**commands** reaches `mcp` only to start the MCP server (`serve`); every other handler reads and writes the graph through `Store`.

Trurlic makes no LLM calls. Design work happens in the agent that calls the MCP tools.

### Store Internals

Graph on disk: `.trurlic/` with `components/`, `decisions/`, `patterns/` subdirectories. Each node is a TOML file. `graph.toml` is a compiled edge index rebuilt deterministically from node files. `.trurlic/.state/` holds the lock file, temp files, the commit counter (`generation`) and the commit journal (`txn.toml`); it is never committed.

Commits: serialize and parse back in memory → stage each file in `.state/tmp/` under a per-process name and flush it → raise `generation` → write and flush the journal, which names every staged file and its target: the commit point → rename the staged files into place, `graph.toml` last, remove deleted files, flush the directories → delete the journal. After the journal a failure returns `CommitPending`; the commit is not rolled back.

Recovery: a leftover journal is applied, then leftover temp files removed, only under the exclusive lock: by every write before it loads, by a command that finds the lock free, and by a watcher that finds a journal.

Content integrity: BLAKE3 hash per node file, stored in `graph.toml`. `trurlic check` verifies hashes. Tamper detection, not encryption.

File locking: std `File::try_lock` (writers, exclusive) and `File::try_lock_shared` (the watcher) on `.state/lock`, polled with a 5 s timeout. `StoreLock` is a proof-of-lock type — write methods require `&StoreLock` as a parameter.

Writer protocol: every write starts at `Store::begin_write`, which takes the caller's state write lock, then the exclusive file lock (dropping the state lock while the file lock is busy), then recovers and reloads the graph from disk. All validation runs against that reloaded state. Every commit raises `.state/generation`; a commit refuses a state whose generation is behind the store's.

In-memory state: `ProjectState` holds `BTreeMap`s of `Arc<ComponentFile>`, `Arc<DecisionFile>`, `Arc<PatternFile>`, plus the `GraphIndex` and an eagerly built `InMemoryGraph` for graph queries.

Thread model: MCP server holds `Arc<RwLock<ProjectState>>`. The file watcher thread (`store::watcher`) detects external changes, loads under the shared file lock, releases it, then swaps state under the write lock (microseconds) unless a write of its own server overtook the load (`ProjectState::is_overtaken`). Events that arrive during a reload start the next one. MCP read tools acquire read lock only. Write tools acquire write lock, then file lock, then reload, validate, and commit. A thread holding a file lock never waits on the state lock.

### Key Invariants

1. `unsafe` is forbidden (`[lints.rust] unsafe_code = "forbid"` in Cargo.toml): no `#[expect]` can lift it.
2. Invariants a tool can check are lints, in Cargo.toml's `[lints]` and `clippy.toml`, and `tests/lints.rs` proves each refuses a seeded violation: no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` or `dbg!` outside tests; no `HashMap`/`HashSet`; stdout and stderr only through `console` (and the MCP transport, which owns stdout under `serve`). An exception is an `#[expect(lint, reason = "...")]` at its site; `#[allow]` is refused.
3. Every graph mutation validates the full graph before touching disk. A write that adds an error is refused, never silently committed; an error the graph already had, matched on kind and subject, does not block it.
4. Atomic commits: round-trip in memory → flushed temp files → flushed journal (the commit point) → renames, `graph.toml` last → directory flushes. Recovery rolls a journal forward under the exclusive lock.
5. File locking prevents concurrent mutations from CLI and MCP processes.
6. Boundary types (`DecisionFile`, `PatternFile`, `ComponentFile`, `GraphIndex`) derive `Serialize + Deserialize`. Internal types (`InMemoryGraph`) do not.
7. Every dependency justified. No proc macros at runtime (serde derive, thiserror are compile-time).
8. Every MCP tool result fits `budget::MAX_TOOL_RESULT_BYTES` (24 KiB) and stays valid JSON; a cut is named in its `truncated` array.

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

Integration tests (`tests/integration/`): drive the built binary. `harness` spawns `trurlic serve` and speaks JSON-RPC over stdio; `golden` compares output with `tests/integration/golden/` (`TRURLIC_UPDATE_GOLDEN=1` rewrites the files). Every tool in `tools/list` is called once.

Failpoints: the `failpoints` cargo feature (test job only) makes `TRURLIC_FAILPOINT=<site>:<n>` act at the n-th hit of a named site in `src/store/failpoint.rs`: abort (or pause, with `TRURLIC_FAILPOINT_PAUSE`) at a `hit`, return an injected I/O error at a `fail`. `make test` enables it.

Property: determinism (every read tool answers with the same bytes in two processes), graph validation catches all known violation classes.

No test for the sake of coverage. Every test asserts a property someone could break.

Benchmarks (`benches/corpus.rs`, criterion, run locally with `make bench`; no CI job): over the seeded corpus `store::corpus` at 50, 600 and 2000 decisions, `load_state`, graph build and validation, and, through `trurlic::bench::Server` in process, `record_decision` end to end and every read tool. The `bench` feature builds the corpus and that server; release builds carry neither.

### Skills

- `rust` — Non-negotiable Rust code rules. Load before any implementation task.
- `review` — Post-implementation quality gate. Load after any implementation task.
- `trurlic` — How to use Trurlic's MCP tools. Load before any task.
