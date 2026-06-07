.PHONY: fmt check test audit audit-js ci setup clean build build-release \
       install-frontend build-frontend fmt-frontend check-frontend test-frontend

FRONTEND_DIR = src/map/frontend
NODE_STAMP   = $(FRONTEND_DIR)/node_modules/.install-stamp

# ── Setup ─────────────────────────────────────────────────────────────────────

setup: install-frontend
	git config core.hooksPath .githooks
	@echo "  ✓ Git hooks installed"

# ── Frontend ──────────────────────────────────────────────────────────────────

# Stamp-file pattern: npm ci only re-runs when package-lock.json changes.
$(NODE_STAMP): $(FRONTEND_DIR)/package-lock.json
	cd $(FRONTEND_DIR) && npm ci
	@touch $@

install-frontend: $(NODE_STAMP)

build-frontend: $(NODE_STAMP)
	cd $(FRONTEND_DIR) && npm run build
	cp $(FRONTEND_DIR)/src/index.html $(FRONTEND_DIR)/dist/
	cp $(FRONTEND_DIR)/src/style.css $(FRONTEND_DIR)/dist/

fmt-frontend: $(NODE_STAMP)
	cd $(FRONTEND_DIR) && npm run fmt

check-frontend: $(NODE_STAMP)
	cd $(FRONTEND_DIR) && npm run fmt:check
	cd $(FRONTEND_DIR) && npm run typecheck

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

# ── Audit ─────────────────────────────────────────────────────────────────────
# Rust:       requires `cargo install cargo-deny`
# TypeScript: npm audit (configured via .npmrc audit-level=high)

audit: audit-js
	cargo deny check

audit-js: $(NODE_STAMP)
	@echo "── TypeScript dependency audit ──────────────────────────────────────"
	cd $(FRONTEND_DIR) && npm audit

# ── CI gate (run before pushing) ──────────────────────────────────────────────

ci: check test audit
	@echo "  ✓ All checks passed"

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf $(FRONTEND_DIR)/node_modules $(FRONTEND_DIR)/dist/app.js
