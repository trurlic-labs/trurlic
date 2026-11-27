.PHONY: install test bench check fmt audit ci setup clean

# ── Install ───────────────────────────────────────────────────────────────────
# Installs to ~/.local/bin/ — same location as install.sh for end users.

install:
	cargo install --path . --locked --root ~/.local
	@echo ""
	@echo "  ✓ trurlic installed to ~/.local/bin/trurlic"

# ── Setup ─────────────────────────────────────────────────────────────────────

setup:
	git config core.hooksPath .githooks
	@echo "  ✓ Git hooks installed"

# ── Format & Lint ─────────────────────────────────────────────────────────────

fmt:
	cargo fmt --all
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged -- -D warnings

check:
	cargo fmt --all -- --check
	cargo clippy --locked --workspace --all-targets -- -D warnings
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# ── Test ──────────────────────────────────────────────────────────────────────

# `failpoints` arms the crash sites in src/failpoint.rs; tests only.
test:
	cargo test --workspace --locked --features failpoints

# ── Bench ─────────────────────────────────────────────────────────────────────

# `bench` builds the seeded corpus and the in-process server the benches
# drive; without it, `cargo bench` has no bench target to run. Benches run
# locally only. Criterion compares each run with the previous one under
# target/criterion. Smoke run, each bench once:
# `cargo bench --features bench -- --test`. The git the benches start reads
# no system or user configuration, so a run times this machine alone.
bench:
	GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null cargo bench --locked --features bench

# ── Audit ─────────────────────────────────────────────────────────────────────
# Requires `cargo install cargo-deny`.

audit:
	cargo deny check

# ── CI gate (run before pushing) ──────────────────────────────────────────────

ci: check test audit
	@echo "  ✓ All checks passed"

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean
