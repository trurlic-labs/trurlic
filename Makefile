.PHONY: fmt check test ci setup clean build build-release

# ── Setup ─────────────────────────────────────────────────────────────────────

setup:
	git config core.hooksPath .githooks
	@echo "  ✓ Git hooks installed"

# ── Build ─────────────────────────────────────────────────────────────────────

build:
	cargo build --locked

build-release:
	cargo build --locked --release

# ── Format & Lint ─────────────────────────────────────────────────────────────

fmt:
	cargo fmt --all
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged -- -D warnings

check:
	cargo fmt --all -- --check
	cargo clippy --locked --workspace --all-targets -- -D warnings

# ── Test ──────────────────────────────────────────────────────────────────────

test:
	cargo test --workspace --locked

# ── CI gate (run before pushing) ──────────────────────────────────────────────

ci: check test
	@echo "  ✓ All checks passed"

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean
