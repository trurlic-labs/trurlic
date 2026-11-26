# Contributing to Trurlic

Trurlic stores architectural decisions and serves them to AI coding agents. Contributions should maintain the same engineering standards the tool itself promotes: intentional decisions, clean code, consistency.

## Reporting bugs and security issues

**Security vulnerabilities** — do not open a public issue. See [SECURITY.md](SECURITY.md).

**Bugs** — open a GitHub issue with your Trurlic version (`trurlic --version`), Rust version (`rustc --version`), platform, and minimal reproduction steps.

**Feature requests** — open an issue describing the use case before writing code. Changes to the `.trurlic/` format, the MCP tool surface, or the decision schema have compatibility implications — wait for maintainer feedback.

## Development setup

**Prerequisites:** Rust 1.90+ (`rustup update stable`), `make`, and `cargo-deny`:

```bash
cargo install cargo-deny
```

**First-time setup:**

```bash
git clone https://github.com/trurlic-labs/trurlic.git
cd trurlic
make setup    # installs the git hooks
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
rustflags = ["-C", "link-arg=-fuse-ld=mold", "-C", "split-debuginfo=unpacked"]
```

## Running checks

```bash
make test       # unit + integration tests
make check      # cargo fmt --check + clippy -D warnings
make audit      # cargo deny check (advisories, licenses, bans, sources)
make ci         # all of the above — run before pushing
make bench      # criterion benches over the seeded corpus (local only, not part of ci)
```

**Benches:** criterion compares each run with the previous one on the same machine, so run `make bench` before and after a change. `cargo bench --features bench -- --test` runs every bench once as a smoke check; `cargo bench --features bench -- load_state` runs one group.

**Single test:**

```bash
cargo test -- a_hand_edited_non_canonical_file_loads_check_flags_it_and_fmt_fixes_it
```

## Making changes

1. Branch from `master`: `git checkout -b feat/my-feature`.
2. Make changes. Add tests for non-trivial behavior.
3. `make fmt` to auto-format.
4. `make ci` to verify.
5. Commit using [conventional commits](#commit-conventions).
6. Open a PR against `master`.

## Working on the store

The `.trurlic/` format is the foundation. Changes to schemas or file operations require extra care:

- Every write is a method of the `Writer` that `Store::begin_write` returns and commits through the journal; a write may add no validation issue the graph did not have.
- Atomic protocol: canonical text (`src/store/format/canonical.rs`, the one serializer of node files) → read back in memory → tmp file → journal → rename.
- A change to the on-disk shape raises `FORMAT_VERSION` (`src/store/model/schema.rs`) and appends the step that reaches it to the migration registry (`src/store/format/migration/steps.rs`), with a fixture test.
- Validation (`src/store/graph/validate.rs`) and the queries (`src/store/graph/query.rs`) are pure: no I/O, testable with in-memory states.

## Working on the MCP server

The MCP server assembles specs from stored decisions.

- The `brief` field uses authoritative language (MUST / DO NOT) — constraints, not suggestions.
- The server keeps no graph between calls: a read tool runs on `Store::load_state`, a write tool on the `Writer` of `Store::begin_write`, so every call pays one load. Keep tool execution fast, and bench a change to the load path with `make bench`.
- Test with a real MCP client to verify response format.

## Commit conventions

[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<scope>): <description>
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `security`.

Scopes: `store`, `mcp`, `budget`, `commands`, `cli`.

```
feat(mcp): add related decisions from connected components to get_context
fix(store): validate component references before writing decision
fix(mcp): keep a cut tool result valid JSON
perf(store): single-pass file I/O in load_state
```

Breaking changes: `BREAKING CHANGE:` in the commit footer.

## Pull request checklist

- [ ] `make ci` passes locally
- [ ] New behavior has tests
- [ ] PR description explains *why*, not just *what*
- [ ] On-disk changes raise `FORMAT_VERSION` and append a migration step

## Code style

`make fmt` handles formatting and auto-fixable lints. Clippy denies warnings. `thiserror` for error types. Every public function documented. The `[lints]` table in `Cargo.toml` and `clippy.toml` refuse `unsafe`, `unwrap()`, `expect()`, `panic!`, `todo!`, `unimplemented!` and `dbg!` outside tests, `HashMap`/`HashSet` (use the BTree collections), and printing anywhere but `console` (`out!`, `diag!`). An exception is `#[expect(lint, reason = "...")]` at its site; `#[allow]` is refused. `tests/lints.rs` checks that each lint fires.
