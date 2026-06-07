.PHONY: fmt check test audit ci setup clean build build-release \
       install-frontend build-frontend fmt-frontend check-frontend test-frontend

FRONTEND_DIR = src/map/frontend

# ── Setup ─────────────────────────────────────────────────────────────────────

setup: install-frontend
	git config core.hooksPath .githooks
	@echo "  ✓ Git hooks installed"

# ── Frontend ──────────────────────────────────────────────────────────────────

install-frontend:
	cd $(FRONTEND_DIR) && npm ci

build-frontend:
	cd $(FRONTEND_DIR) && npx esbuild src/main.ts \
		--bundle --outfile=dist/app.js --format=iife --target=es2020 --minify
	cp $(FRONTEND_DIR)/src/index.html $(FRONTEND_DIR)/dist/
	cp $(FRONTEND_DIR)/src/style.css $(FRONTEND_DIR)/dist/

fmt-frontend:
	cd $(FRONTEND_DIR) && npx prettier --write 'src/**/*.{ts,css,html}'

check-frontend:
	cd $(FRONTEND_DIR) && npx prettier --check 'src/**/*.{ts,css,html}'
	cd $(FRONTEND_DIR) && npx tsc --noEmit

test-frontend: check-frontend

# ── Build ─────────────────────────────────────────────────────────────────────

build:
	cargo build --locked

build-release: build-frontend
	cargo build --locked --release

# ── Format & Lint ─────────────────────────────────────────────────────────────

fmt: fmt-frontend
	cargo fmt --all
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged -- -D warnings

check: check-frontend
	cargo fmt --all -- --check
	cargo clippy --locked --workspace --all-targets -- -D warnings

# ── Test ──────────────────────────────────────────────────────────────────────

test: test-frontend
	cargo test --workspace --locked

# ── Audit (requires: cargo install cargo-deny) ───────────────────────────────

audit:
	cargo deny check

# ── CI gate (run before pushing) ──────────────────────────────────────────────

ci: check test audit
	@echo "  ✓ All checks passed"

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -f $(FRONTEND_DIR)/dist/app.js
