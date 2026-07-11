# Contributing to Trurlic

Trurlic stores architectural decisions and serves them to AI coding agents. Contributions should maintain the same engineering standards the tool itself promotes: intentional decisions, clean code, consistency.

## Reporting bugs and security issues

**Security vulnerabilities** — do not open a public issue. See [SECURITY.md](SECURITY.md).

**Bugs** — open a GitHub issue with your Trurlic version (`trurlic --version`), Rust version (`rustc --version`), platform, and minimal reproduction steps.

**Feature requests** — open an issue describing the use case before writing code. Changes to the `.trurlic/` format, the MCP tool surface, or the decision schema have compatibility implications — wait for maintainer feedback.

## Development setup

**Prerequisites:** Rust 1.88+ (`rustup update stable`), `make`, and `cargo-deny`:

```bash
cargo install cargo-deny
```

**First-time setup:**

```bash
git clone https://github.com/trurlic-labs/trurlic.git
cd trurlic
make setup    # installs frontend deps + git hooks
cargo build
cargo test
```

**Faster linker (optional, Linux):**

```bash
sudo apt install clang mold
```

Add to `~/.cargo/config.toml`:

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

## Running checks

```bash
make test       # unit + integration tests
make check      # cargo fmt --check + clippy -D warnings
make audit      # cargo deny check (advisories, licenses, bans, sources)
make ci         # all of the above — run before pushing
```

**Single test:**

```bash
cargo test -- remove_decision_allows_with_constrains_edge
```

## Making changes

1. Branch from `main`: `git checkout -b feat/my-feature`.
2. Make changes. Add tests for non-trivial behavior.
3. `make fmt` to auto-format.
4. `make ci` to verify.
5. Commit using [conventional commits](#commit-conventions).
6. Open a PR against `main`.

## Working on the store

The `.trurlic/` format is the foundation. Changes to schemas or file operations require extra care:

- All writes use `commit_with_graph` — validates the full graph before touching disk.
- Atomic protocol: serialize → tmp file → verify round-trip → rename. `graph.toml` renamed last.
- Schema changes must bump `FORMAT_VERSION` in `schema.rs`.
- The graph module (`graph.rs`) is pure — no I/O, fully testable with in-memory fixtures.

## Working on the MCP server

The MCP server assembles specs from stored decisions.

- The `brief` field uses authoritative language (MUST / DO NOT) — constraints, not suggestions.
- Write tools validate input sizes, check reserved names, and run full graph validation before committing.
- The server uses `Arc<RwLock<ProjectState>>` shared with a file watcher thread. Tool calls hold the write lock. Keep tool execution fast (<50ms for writes).
- Test with a real MCP client to verify response format.

## Working on the workflow engine

`workflow` computes the next step for the coding agent — the `advance` state machine and the step prompts MCP serves.

- `advance()` is a pure function of `(graph, now)` — no I/O, no LLM calls, no wall-clock reads, no side effects. Same inputs must always produce the same step. Inject `Utc::now()` at the call site; never read it inside the engine.
- Steps have preconditions (what the graph must look like) and postconditions (what changes after the step succeeds). The state machine gates on `completed_steps` so it can't loop on a step whose postcondition it can't detect.
- Prompts are transport-agnostic and mode-aware — the same text feeds MCP and any other caller. Agent-mode variants instruct autonomous code reading; interactive variants preserve the Socratic ask-then-record dialogue.
- Every `Step::as_str()` value must be accepted by `build_step_prompt()`, and every `(Mode, TaskType)` pair must be resolved explicitly. These are property-tested — extend the tests when you add a variant.

## Commit conventions

[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<scope>): <description>
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `security`.

Scopes: `store`, `workflow`, `mcp`, `map`, `commands`, `cli`.

```
feat(mcp): add related decisions from connected components to get_context
fix(store): validate component references before writing decision
fix(workflow): gate CoverConcerns on completed_steps to stop advance looping
perf(store): single-pass file I/O in load_state
```

Breaking changes: `BREAKING CHANGE:` in the commit footer.

## Pull request checklist

- [ ] `make ci` passes locally
- [ ] New behavior has tests
- [ ] PR description explains *why*, not just *what*
- [ ] Schema changes bump `FORMAT_VERSION`

## Code style

`make fmt` handles formatting and auto-fixable lints. Clippy denies warnings. `thiserror` for error types. Every public function documented. No `unwrap()` or `expect()` in production code — the crate enforces this via `#![deny(clippy::unwrap_used, clippy::expect_used)]`.
