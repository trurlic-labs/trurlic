## Trurlic

Architecture layer for AI-assisted codebases. Typed decision graph stored in `.trurlic/`, served to coding agents over MCP. Socratic design conversations, concern tracking, pattern detection, comprehension gates.

Named after Trurl (Stanisław Lem, *The Cyberiad*) — the constructor who thinks deeply about what he builds before building it.

### Architecture

Single crate, eight modules. Visibility enforces boundaries — `pub(crate)` on everything except `cli` and `store`.

```
store       → (no internal deps)         Decision graph: TOML files, graph index,
                                          validation, atomic writes, file locking
workflow    → store                       Step deduction, concern tracking,
                                          prompt generation. Pure functions, no I/O.
mcp         → store, workflow             MCP server: JSON-RPC stdio, tool dispatch,
                                          context assembly, file watcher
session     → store, workflow, provider   CLI design sessions, bootstrap driver,
                                          LLM extraction, session persistence
commands    → store, session, config      CLI command handlers
map         → store                       Interactive graph visualization,
                                          WebSocket live sync, REST API
provider    → (no internal deps)          LLM API clients (Anthropic, OpenAI,
                                          OpenRouter), SSE streaming
config      → (no internal deps)          Provider resolution, API key handling
```

**store** is the foundation. It never imports from any other module. Every write goes through `Store` methods with `StoreLock` proof parameters.

**workflow** is pure computation. It never calls an LLM, never touches the filesystem, never allocates beyond the response JSON. `advance()` is a deterministic function of graph state + inputs. Same inputs = same output, always.

**mcp** never writes to the graph directly. It calls `Store` write methods. It never runs LLM calls. Prompt generation comes from `workflow::steps`.

**session** is the only module that calls LLM APIs. It owns the CLI dialogue loop and the bootstrap driver.

**provider** and **config** are leaf modules. They do not import from any other internal module.

### Store Internals

Graph on disk: `.trurlic/` with `components/`, `decisions/`, `patterns/` subdirectories. Each node is a TOML file. `graph.toml` is a compiled edge index rebuilt deterministically from node files.

Atomic writes: serialize → write to temp file → verify round-trip parse → rename into place. `graph.toml` renamed last as the commit point. Interrupted writes cleaned up on next startup.

Content integrity: BLAKE3 hash per node file, stored in `graph.toml`. `trurlic check` verifies hashes. Tamper detection, not encryption.

File locking: `fs2::FileExt` for cross-platform flock. `StoreLock` is a proof-of-lock type — write methods require `&StoreLock` as a parameter. The lock is never held across LLM calls.

In-memory state: `ProjectState` holds `BTreeMap`s of `Arc<ComponentFile>`, `Arc<DecisionFile>`, `Arc<PatternFile>`, plus the `GraphIndex`. Graph queries go through a cached `InMemoryGraph` behind `OnceLock`.

Thread model: MCP server holds `Arc<RwLock<ProjectState>>`. File watcher thread detects external changes and swaps state under write lock (microseconds). MCP read tools acquire read lock only. Write tools acquire write lock, then file lock, then validate, then commit.

### Workflow Engine

`advance()` is the orchestration hub. Read-only, stateless, idempotent. Computes the next step from graph contents every call. No session tracking, no persistent workflow state.

Seven task types, each with a distinct step sequence. Steps have preconditions (graph must look like X) and postconditions (graph changes after step succeeds). The state machine checks preconditions to determine the next step.

Concern tracking: 10 architectural concern areas with keyword matching against decision content. Priority-ordered — security gaps surface before stylistic ones.

Step prompts: transport-agnostic instructions generated from graph state. MCP and CLI sessions consume the same prompts. The INTERACTION_PROTOCOL is embedded in every interactive step prompt.

### Key Invariants

1. `unsafe` is denied (`[lints.rust] unsafe_code = "deny"` in Cargo.toml)
2. `unwrap()` and `expect()` denied outside `#[cfg(test)]` (`#![cfg_attr(not(test), deny(...))]`)
3. Every graph mutation validates the full graph before touching disk. Invalid writes refused, never silently committed.
4. Atomic writes: serialize → temp → verify round-trip → rename. `graph.toml` renamed last.
5. API keys wrapped in `Zeroizing<String>`. Zeroed from memory on drop. Never logged, never in error messages. Display/Debug show only last 4 characters.
6. File locking prevents concurrent mutations from CLI + MCP + map.
7. `workflow::advance` is a pure function. No I/O, no LLM calls, no side effects.
8. Boundary types (`Decision`, `Pattern`, `Component`, `GraphIndex`) derive `Serialize + Deserialize`. Internal types (`InMemoryGraph`, `Engine`) do not.
9. Every dependency justified. No proc macros at runtime (serde derive, thiserror are compile-time).

### Trurlic

This project uses Trurlic for its own architectural decisions. Two modes.

**Design mode — advance loop.** Use when the task requires new architectural decisions: new component, new concern area, design change. The advance loop is interactive — gated steps require user involvement.

```
1. advance(component, task_type) → { step, action, requires_user_input }
2. follow the action (get_step_prompt, add_component, etc.)
3. if requires_user_input: ask the user, wait, pass their response
4. advance again
5. repeat until ready: true
6. get_context → implement constrained by the brief
```

**Implementation mode — get_context directly.** Use when implementing within existing constraints. No advance, no gates, fully autonomous.

```
1. get_context(component) → brief with all decisions and constraints
2. implement within the brief
3. if undecided pattern encountered:
   check_pattern(description) → if uncovered:
     record_decision(component, choice, reason, attribution="agent")
     continue — the decision is flagged ⚠ for human review
4. get_context(component, depth="constraints") → verify compliance
```

When to use which: if the task says "add a feature," "fix a bug," or "implement X" and the component has existing decisions that cover the work, use implementation mode. If the task says "design," "architect," "add a new component," or you realize the existing decisions don't cover what you need to do, switch to design mode.

During implementation in either mode:
- When touching a second module, call `get_context` for that module's component too.
- After implementation, re-read the brief and verify no decision was silently violated.

### Testing

Unit tests: pure functions, same file. Every module has exhaustive tests for its public contract.

Integration tests: full pipeline runs — advance through all steps for every task type, verify step sequences and postconditions. Schema round-trips for every serializable type.

Property: determinism (same graph state → same advance result), exhaustive step coverage (every `Step::as_str()` value accepted by `build_step_prompt()`), graph validation catches all known violation classes.

No test for the sake of coverage. Every test asserts a property someone could break.

Benchmarks (criterion + codspeed): `Store::load_state()` vs graph size.

### Skills

- `rust` — Non-negotiable Rust code rules. Load before any implementation task.
- `review` — Post-implementation quality gate. Load after any implementation task.
- `trurlic` — How to use Trurlic's MCP tools and advance loop. Load before any task.
