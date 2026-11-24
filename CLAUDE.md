## Trurlic

Architecture layer for AI-assisted codebases. Typed decision graph stored in `.trurlic/` as one TOML file per node, served to coding agents over MCP: the decisions and project-wide rules that constrain a component or a file before code is written, with proposed decisions marked until the owner accepts them.

Named after Trurl (Stanisław Lem, *The Cyberiad*) — the constructor who thinks deeply about what he builds before building it.

### Architecture

Single crate, seven modules (`src/lib.rs`). Visibility enforces boundaries — `pub(crate)` on everything except `cli` and `store`.

```
store       → (no internal deps)         Decision graph: TOML node files, their
                                          canonical form, validation, atomic
                                          writes, file locking, migrations
mcp         → store, budget               MCP server: JSON-RPC stdio, version
                                          negotiation, the server's own requests,
                                          argument specs, tool dispatch, context
                                          assembly, the 24 KiB cap on every tool
                                          result
commands    → store, mcp                  CLI command handlers: init, query,
                                          status, check, fmt, migrate, install
                                          (registers the MCP server with Claude
                                          Code), and the `serve` entry point
cli         → commands                    clap definitions and dispatch
budget      → (no internal deps)          Fitting output into a byte budget: the
                                          level search behind the result cap
error       → store (types)               The crate's `Error` enum and `Result`
                                          alias; it names the store's anchor-fault
                                          and timestamp-fault types
console     → (no internal deps)          Terminal I/O: `out!` to stdout, `diag!`
                                          to stderr
```

Every module uses `error` and `console`; the arrows leave them out.

**store** is the foundation. It imports no other module except `error` and `console`. Every graph write is a method of the `Writer` that `Store::begin_write` returns, which owns the exclusive lock and the graph loaded under it; the commit path takes `&StoreLock` as proof of the lock. Validation holds every decision's choice and reason to `store::text`, whoever wrote them, a hand edit included.

**error** carries the path of the file behind every I/O and TOML failure (`Error::Io`, `Error::Toml`); nothing converts a bare `io::Error` into it, so a `?` cannot drop the path. `commands::install` alone returns its own `InstallError`, which wraps `Error`; `cli` prints either as `error: <message>` with exit status 1.

**mcp** never writes to the graph directly: a write tool runs on a `Writer` (none is listed yet; both tools are reads), and the server holds no graph between calls. A tool is built on an argument spec (`mcp::spec`): its `Args::declare` is walked once for the `inputSchema` and once per call for the parse, so the schema and the parser cannot drift, and an unknown key, a wrong type or a broken limit is refused naming the field. Its `Access` (read, write, interactive) decides the lock the call takes. Every refusal, a missing store included, is a tool result with `isError: true` whose text is `{"error": {"kind", "field"?, "message"}}` (`mcp::result`); a JSON-RPC error answers only a malformed message, an unknown method or tool, or a panic. The two tools that predate specs, `get_context` and `get_decisions_for_file`, keep their hand-written schemas in `mcp::tools` until `brief` replaces them.

**mcp transport**: `initialize` answers the client's protocol revision when it is in `PROTOCOL_VERSIONS`, else the latest, and keeps the client's capabilities. During a tool call a handler can send the client a request through its `Peer` (`mcp::peer`) and wait for the answer on the same stdin, without a timeout: meanwhile a `ping` is answered, other requests are queued (16 at most) and served after the call, and `notifications/cancelled` for the call ends the wait with no response. Every line gets at most one answer; a line over 10 MiB is discarded whole.

**commands** reaches `mcp` only to start the MCP server (`serve`); every other handler reads and writes the graph through `Store`. Until `apply` lands, a write of a node is a hand edit of its file, which every load validates; `trurlic fmt` is the one command that writes graph files.

Trurlic makes no LLM calls. Design work happens in the agent that calls the MCP tools.

### Store Internals

Graph on disk (format 0.5): `.trurlic/project.toml` (`format_version`, `name`, `description`, `placement = "tracked" | "shared"`), `components/<name>.toml` (`description`, `connects_to`) and `decisions/<name>.toml` (`component`, `choice`, `reason`, `alternatives`, `tags`, `status = "proposed" | "accepted"`, `accepted_via`, `origin`, `created`, `anchors = [{file, symbol?}]`, `depends_on`, and an `[accepted_text]` table while a revision of an accepted decision is proposed). The file stem is the node's name; no file holds it, and a stem is unique across node kinds. Each edge lives in the file of its source; `project` is the component of the project-wide decisions and has no file. There is no index, no pattern, no history and no hash in the graph. `.trurlic/.state/` holds the lock file, temp files and the commit journal (`txn.toml`); it is never committed.

Types (`store/schema.rs`) refuse every key they do not know and every value they cannot hold: `accepted_via` on a proposed decision, `accepted_text` on an accepted one, an anchor path that is absolute, holds `..`, a backslash, an empty or `.` segment.

Load and validation: a load (`store/load.rs`) fails only when a file cannot be read as its type: it cannot be read, it is over 1 MiB (refused before it is parsed), it is not TOML, or its type refuses it; the error names the file. Everything a loaded graph can hold but should not is an issue `ProjectState::validate` reports with the file it is in: a stem that is not kebab-case or is `project`, a stem a component and a decision share, a component, `connects_to` or `depends_on` that names no node of its kind, a self-edge, a `depends_on` cycle, a choice or reason that breaks `store::text`. `check` fails on an issue; `status` and the reads still answer.

Canonical form (`store/canonical.rs`): the one serializer of node files and `project.toml`. Keys in a fixed order per file type; sets (tags, edges, anchors) sorted, one element per line with a trailing comma, so two branches that add elements at different places merge; an empty set or list left out; a string with a line break as a `"""` string. The form is Trurlic's, not the TOML library's printer. Every load notes the files whose bytes are not canonical, comparing as it writes without allocating; `check` warns about each and `trurlic fmt` rewrites exactly those in one commit.

Commits: write the canonical text and parse it back in memory, refused unless it reads as the value written → stage each file in `.state/tmp/` under a per-process name and flush it → write and flush the journal, which names every staged file, its target and the BLAKE3 of its bytes: the commit point → rename the staged files into place, remove deleted files, flush the directories → delete the journal. After the journal a failure returns `CommitPending`; the commit is not rolled back.

Recovery: a leftover journal is applied, then leftover temp files removed, only under the exclusive lock: by every write before it loads, and by every read that finds either one under the shared lock.

Timestamps: `store::Timestamp`, a UTC instant to the nanosecond, written as an RFC 3339 string in the form 0.3 releases wrote (`2025-06-01T10:30:00Z`, a fraction of 0, 3, 6 or 9 digits), so a load and a write give back the same bytes. `Timestamp::now` is the crate's one clock read.

Format version: `project.toml` states `format_version`, `MAJOR.MINOR.PATCH` and independent of the crate's version (`store::FormatVersion`, `FORMAT_VERSION` in `store/schema.rs`). The check has one home, `store/version.rs`: every read and every write refuses another format before it recovers anything or parses a node file. A newer one says to upgrade trurlic, an older one the registry names to run `trurlic migrate`, a version the registry does not name, or one no chain of steps leads from, that no migration leads to this format (`Error::NoMigration`), and a version that does not parse is named with its file and its text. `store::migration` holds the registry, one step per format change from 0.1.0 on; `trurlic migrate` walks the chain from the store's version, reports every value a step drops, reads the result as the current types, which refuse any key they do not know, and lands it in one journaled commit that writes each file whose canonical bytes change, `project.toml` first, and removes each file the steps dropped. A migration stopped before that rename leaves an old-format store with a journal, which only the next `migrate` finishes; once it has happened, the store is current and the next command of any kind finishes it.

File locking: std `File::try_lock` (writers, exclusive) and `File::try_lock_shared` (readers) on `.state/lock`, polled with a 5 s timeout. `StoreLock` is a proof-of-lock type: every function that changes a file under `.trurlic/` takes `&StoreLock`, and a `Writer` owns one.

Writer protocol: every write starts at `Store::begin_write`, which takes the exclusive file lock, checks the format, recovers, loads the graph from disk and returns a `Writer` that owns the lock and that graph. Every graph write is a `Writer` method, so all validation runs against the graph loaded under the lock, and each commit leaves the writer's graph equal to disk. Dropping the writer releases the lock.

Readers: every read (`Store::load_state`) takes the shared lock, checks the format and loads the graph. A journal or a temp file seen under the shared lock was left by a writer that stopped, so the read takes the exclusive lock, finishes that commit and loads under it: no read sees half of a commit.

In-memory state: `ProjectState` holds `BTreeMap`s of `Component` and `Decision` by stem, the `Project`, and the files that are not in canonical form; the queries of `store/query.rs` walk the maps. It lives for one call.

No state between calls: the MCP server holds the directory it was started in, and every tool call finds the store from there and loads it. A read tool runs on `Store::load_state`, a write tool on a `Writer`, so a hand edit, a `git checkout` or another process's commit is seen by the very next call. `serve` starts without a store or with one in another format, and each call is refused until it is fixed, naming `trurlic init`, `trurlic migrate`, or the file and text of a version that does not parse. `load_state` reads the node files in parallel, each in one bounded read; `make bench` times it.

### Key Invariants

1. `unsafe` is forbidden (`[lints.rust] unsafe_code = "forbid"` in Cargo.toml): no `#[expect]` can lift it.
2. Invariants a tool can check are lints, in Cargo.toml's `[lints]` and `clippy.toml`, and `tests/lints.rs` proves each refuses a seeded violation: no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` or `dbg!` outside tests; no `HashMap`/`HashSet`; stdout and stderr only through `console` (and the MCP transport, which owns stdout under `serve`); the clock only through `Timestamp::now`. An exception is an `#[expect(lint, reason = "...")]` at its site; `#[allow]` is refused.
3. A write adds no issue: the graph it commits has no issue, matched on kind and subject, that the graph it loaded lacked (`ProjectState::introduced_errors`). `fmt` commits only canonical text that reads back as the value loaded, so the graph it commits is the one it loaded.
4. Atomic commits: canonical text read back in memory → flushed temp files → flushed journal (the commit point) → renames → directory flushes. Recovery rolls a journal forward under the exclusive lock.
5. File locking prevents concurrent mutations from CLI and MCP processes.
6. What a call returns is a function of the files on disk: no process holds graph state between calls (`tests/integration/stdio.rs` sees a hand edit in the next call).
7. Node files are read only as their types, with `deny_unknown_fields`, after a read capped at `MAX_NODE_FILE_BYTES` (1 MiB), and written only in the canonical form: the types derive `Deserialize` and no `Serialize`, so no other serializer can write one.
8. Every dependency justified. No proc macros at runtime (serde derive, thiserror are compile-time).
9. Every MCP tool result fits `budget::MAX_TOOL_RESULT_BYTES` (24 KiB) and stays valid JSON; a cut is named in its `truncated` array. Every tool description fits 2,048 characters (`mcp::tools` tests).
10. A panic while a request is handled is answered `-32603` and the server serves the next request: release builds unwind (`panic = "unwind"`), `mcp::Server::answer` is the one `catch_unwind`, and nothing outlives a request half-done (no graph is held; unwinding drops a writer's lock; every line is built before it is written). `tests/integration/server_failpoints.rs` panics under the shared lock, then a writer takes the lock and the server answers again.

### Trurlic

This project uses Trurlic for its own architectural decisions. Before a
task, read the decisions that constrain it; implement within them; record
the choices they leave open; check before committing.

```
1. get_context(component) → brief with every decision and project rule
   the code must respect; get_decisions_for_file(path) for a file's own
2. implement within the brief
3. a choice no decision covers: search .trurlic/decisions/; if none
   covers it, write .trurlic/decisions/<name>.toml by hand
4. trurlic fmt, then trurlic check: no issue before committing
```

- When touching a second module, call `get_context` for that module's component too.
- Status: `accepted` when the user made the choice in this conversation, `proposed` when you made it alone; a proposed decision is marked in every brief until the user accepts it.
- When the task is to design something, discuss the choices with the user before implementing, and record each one they agree on.
- After implementation, re-read the brief and check no decision was silently violated.

### Testing

Unit tests: pure functions, same file. Every module has exhaustive tests for its public contract. Every file type reads back from its canonical text as the value written.

Integration tests (`tests/integration/`): drive the built binary. `harness` spawns `trurlic serve` and speaks JSON-RPC over stdio, playing the client for the server's own requests too; `golden` compares output with `tests/integration/golden/` (`TRURLIC_UPDATE_GOLDEN=1` rewrites the files). Every tool in `tools/list` is called once. `stdio.rs` covers each protocol case: version negotiation, each malformed line, an oversize line, an unknown tool, a server with no store or an old one. `malformed_node.rs` has one test per shape a hand edit can break; `canonical_form.rs` holds `check` and `fmt` to a hand edit; `git_merge.rs` merges two branches of a tracked graph in a real git repository.

Argument specs: `mcp::spec` holds one table-driven test over a spec with every field kind the 0.4.0 tools need, refusing each malformed shape with its field named.

Failpoints: the `failpoints` cargo feature (test job only) makes `TRURLIC_FAILPOINT=<site>:<n>` act at the n-th hit of a named site in `src/store/failpoint.rs`: abort (or pause, with `TRURLIC_FAILPOINT_PAUSE`) at a `hit`, return an injected I/O error at a `fail`, panic at a `panic` (`graph.loaded`, under the shared or the exclusive lock), and let the caller act at a `triggers` site (`mcp.server_request`, where a tool call sends the client an elicitation form instead of running). `make test` enables it.

Property: determinism (every read tool answers with the same bytes in two processes), graph validation catches all known violation classes, and every node of the corpus reads back from its canonical bytes, which are a fixpoint.

Format fixtures (`tests/fixtures/formats/<version>/`): a graph written by the trurlic of each historical format. Each one migrates to the current format, loads and passes `check` in a named test, its report and result pinned by a golden file; those tests are ignored until the step from 0.4.1 to 0.5.0 is in the registry.

No test for the sake of coverage. Every test asserts a property someone could break.

Benchmarks (`benches/corpus.rs`, criterion, run locally with `make bench`; no CI job): over the seeded corpus `store::corpus` at 50, 600 and 2000 decisions, `load_state`, validation, and, through `trurlic::bench::Server` in process, every read tool, each with the load every call makes. The `bench` feature builds the corpus and that server; release builds carry neither.

### Skills

- `rust` — Non-negotiable Rust code rules. Load before any implementation task.
- `review` — Post-implementation quality gate. Load after any implementation task.
- `trurlic` — How to use Trurlic's MCP tools. Load before any task.
