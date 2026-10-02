## Trurlic

Architecture layer for AI-assisted codebases. Typed decision graph stored in `.trurlic/`, served to coding agents over MCP. Socratic design conversations, concern tracking, pattern detection, comprehension gates.

Named after Trurl (Stanisław Lem, *The Cyberiad*) — the constructor who thinks deeply about what he builds before building it.

### Architecture

Single crate, seven modules (`src/lib.rs`). Visibility enforces boundaries — `pub(crate)` on everything except `cli` and `store`.

```
store       → (no internal deps)         Decision graph: TOML files, graph index,
                                          validation, atomic writes, file locking,
                                          file watcher
workflow    → store                       Step deduction, concern tracking,
                                          prompt generation. Pure functions, no I/O.
mcp         → store, workflow             MCP server: JSON-RPC stdio, tool dispatch,
                                          context assembly, decision verification
map         → store                       Interactive graph visualization,
                                          WebSocket live sync, REST API
commands    → store, mcp, map,            CLI command handlers: init, add, rename,
              workflow (read-only)        remove, decide, query, status, check, gc,
                                          migrate, install (IDE MCP config), and
                                          the `serve` and `map` entry points
cli         → commands                    clap definitions and dispatch
error       → (no internal deps)          The crate's single `Error` enum and
                                          `Result` alias
```

Every module uses `error`; the arrows leave it out.

**store** is the foundation. It imports no other module except `error`. Every write goes through `Store` methods with `StoreLock` proof parameters.

**workflow** is pure computation. It never touches the filesystem, never allocates beyond the response JSON. `advance()` is a deterministic function of graph state + inputs. Same inputs = same output, always.

**mcp** never writes to the graph directly. It calls `Store` write methods. Prompt generation comes from `workflow::steps`.

**commands** reads `workflow::concerns` to report the concern coverage a removal loses (`remove decision`, `gc`), never mutating the graph through it, and starts the MCP server (`serve`) and the map server (`map`).

Trurlic makes no LLM calls. Design work happens in the agent that calls the MCP tools.

### Store Internals

Graph on disk: `.trurlic/` with `components/`, `decisions/`, `patterns/` subdirectories. Each node is a TOML file. `graph.toml` is a compiled edge index rebuilt deterministically from node files. `.trurlic/.state/` holds the lock file, temp files, the commit counter (`generation`), the commit journal (`txn.toml`) and map layout; it is never committed.

Commits: serialize and parse back in memory → stage each file in `.state/tmp/` under a per-process name and flush it → raise `generation` → write and flush the journal, which names every staged file and its target: the commit point → rename the staged files into place, `graph.toml` last, remove deleted files, flush the directories → delete the journal. After the journal a failure returns `CommitPending`; the commit is not rolled back.

Recovery: a leftover journal is applied, then leftover temp files removed, only under the exclusive lock: by every write before it loads, by a command that finds the lock free, and by a watcher that finds a journal.

Content integrity: BLAKE3 hash per node file, stored in `graph.toml`. `trurlic check` verifies hashes. Tamper detection, not encryption.

File locking: std `File::try_lock` (writers, exclusive) and `File::try_lock_shared` (watchers) on `.state/lock`, polled with a 5 s timeout. `StoreLock` is a proof-of-lock type — write methods require `&StoreLock` as a parameter.

Writer protocol: every write starts at `Store::begin_write`, which takes the caller's state write lock, then the exclusive file lock (dropping the state lock while the file lock is busy), then recovers and reloads the graph from disk. All validation runs against that reloaded state. Every commit raises `.state/generation`; a commit refuses a state whose generation is behind the store's.

In-memory state: `ProjectState` holds `BTreeMap`s of `Arc<ComponentFile>`, `Arc<DecisionFile>`, `Arc<PatternFile>`, plus the `GraphIndex` and an eagerly built `InMemoryGraph` for graph queries.

Thread model: MCP server holds `Arc<RwLock<ProjectState>>`. File watcher thread detects external changes, loads under the shared file lock, releases it, then swaps state under the write lock (microseconds) unless a write of its own server overtook the load (`ProjectState::is_overtaken`). Events that arrive during a reload start the next one. MCP read tools acquire read lock only. Write tools acquire write lock, then file lock, then reload, validate, and commit. A thread holding a file lock never waits on the state lock.

### Workflow Engine

`advance()` is the orchestration hub. Read-only, stateless, idempotent. Computes the next step from graph contents every call. No session tracking, no persistent workflow state.

Seven task types, each with a distinct step sequence. Steps have preconditions (graph must look like X) and postconditions (graph changes after step succeeds). The state machine checks preconditions to determine the next step.

Concern tracking: 10 architectural concern areas with keyword matching against decision content. Priority-ordered — security gaps surface before stylistic ones.

Step prompts: transport-agnostic instructions generated from graph state and served by `get_step_prompt`. Interactive mode embeds `INTERACTION_PROTOCOL` in every step prompt; agent mode embeds `AGENT_PROTOCOL`.

### Key Invariants

1. `unsafe` is denied (`[lints.rust] unsafe_code = "deny"` in Cargo.toml)
2. `unwrap()` and `expect()` denied outside `#[cfg(test)]` (`#![cfg_attr(not(test), deny(...))]`)
3. Every graph mutation validates the full graph before touching disk. A write that adds an error is refused, never silently committed; an error the graph already had, matched on kind and subject, does not block it.
4. Atomic commits: round-trip in memory → flushed temp files → flushed journal (the commit point) → renames, `graph.toml` last → directory flushes. Recovery rolls a journal forward under the exclusive lock.
5. File locking prevents concurrent mutations from CLI + MCP + map.
6. `workflow::advance` is a pure function. No I/O, no side effects.
7. Boundary types (`DecisionFile`, `PatternFile`, `ComponentFile`, `GraphIndex`) derive `Serialize + Deserialize`. Internal types (`InMemoryGraph`) do not.
8. Every dependency justified. No proc macros at runtime (serde derive, thiserror are compile-time).

### Trurlic

This project uses Trurlic for its own architectural decisions.

**Before any task**, call `advance(component, task_type)`. If you haven't
specified a mode, advance will ask — present the choice to the user:

- **agent**: AI reads code and makes decisions autonomously. Fast,
  no user interaction needed. Decisions flagged for later review.
- **interactive**: User participates in design discussion. Slower,
  but builds shared understanding.

Then follow the returned action. Call advance again after each step.
Repeat until `ready: true`. Then get_context → implement.

When to suggest which mode:
- "implement X" / "fix Y" / "add feature Z" → suggest agent
- "design" / "architect" / "let's think about" → suggest interactive
- When uncertain → ask

**Implementation mode — get_context directly.** Use when implementing within existing constraints. No advance, no gates, fully autonomous.

```
1. get_context(component) → brief with all decisions and constraints
2. implement within the brief
3. if undecided pattern encountered:
   check_pattern(description) → if uncovered:
     record_decision(component, choice, reason, attribution="agent")
     continue — the decision is flagged ⚠ for human review
4. get_context(component, depth="constraints") → verify compliance
5. verify_against_decisions(component, changed_files) → read each
   verdict, fix any VIOLATED decision before committing
```

When to use which: if the task says "add a feature," "fix a bug," or "implement X" and the component has existing decisions that cover the work, use implementation mode. If the task says "design," "architect," "add a new component," or you realize the existing decisions don't cover what you need to do, switch to design mode.

During implementation in either mode:
- When touching a second module, call `get_context` for that module's component too.
- After implementation, re-read the brief and verify no decision was silently violated.
- After implementing, call `verify_against_decisions` with the component and changed files. Review each verdict. Fix violations before committing.

### Testing

Unit tests: pure functions, same file. Every module has exhaustive tests for its public contract.

Pipeline tests: advance through all steps for every task type, verify step sequences and postconditions. Schema round-trips for every serializable type.

Integration tests (`tests/integration/`): drive the built binary. `harness` spawns `trurlic serve` and speaks JSON-RPC over stdio; `golden` compares output with `tests/integration/golden/` (`TRURLIC_UPDATE_GOLDEN=1` rewrites the files). Every tool in `tools/list` is called once.

Failpoints: the `failpoints` cargo feature (test job only) makes `TRURLIC_FAILPOINT=<site>:<n>` act at the n-th hit of a named site in `src/store/failpoint.rs`: abort (or pause, with `TRURLIC_FAILPOINT_PAUSE`) at a `hit`, return an injected I/O error at a `fail`. `make test` enables it.

Property: determinism (same graph state → same advance result), exhaustive step coverage (every `Step::as_str()` value accepted by `build_step_prompt()`), graph validation catches all known violation classes.

No test for the sake of coverage. Every test asserts a property someone could break.

Benchmarks (criterion + codspeed): `Store::load_state()` vs graph size.

### Skills

- `rust` — Non-negotiable Rust code rules. Load before any implementation task.
- `review` — Post-implementation quality gate. Load after any implementation task.
- `trurlic` — How to use Trurlic's MCP tools and advance loop. Load before any task.
