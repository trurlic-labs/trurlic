.PHONY: install build build-debug build-release build-frontend install-frontend \
       test test-frontend check check-frontend \
       fmt fmt-frontend audit audit-js ci setup clean

FRONTEND_DIR = src/map/frontend
NODE_STAMP   = $(FRONTEND_DIR)/node_modules/.install-stamp

# ── Install (single command: rebuild everything → install binary) ──────────
# Installs to ~/.local/bin/ — same location as install.sh for end users.

install: build-frontend
	@touch src/map/embed.rs
	cargo install --path . --locked --root ~/.local
	@echo ""
	@echo "  ✓ trurlic installed to ~/.local/bin/trurlic"

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

fmt-frontend: $(NODE_STAMP)
	cd $(FRONTEND_DIR) && npm run fmt

check-frontend: $(NODE_STAMP)
	cd $(FRONTEND_DIR) && npm run fmt:check
	cd $(FRONTEND_DIR) && npm run typecheck

test-frontend: check-frontend
	cd $(FRONTEND_DIR) && npm test

# ── Build ─────────────────────────────────────────────────────────────────────
# `make build` is the default dev workflow: build frontend + install binary
# so `trurlic` always points to the latest version.

build: install

build-debug: build-frontend
	cargo build --locked

build-release: build-frontend
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

# ── Audit ─────────────────────────────────────────────────────────────────────
# Rust:       requires `cargo install cargo-deny`
# TypeScript: cve-lite-cli + npm audit + lockfile-lint

audit: audit-js
	cargo deny check

audit-js: $(NODE_STAMP)
	@echo "── npm dependency CVE scan ──────────────────────────────────────────"
	# TODO(2026-06-18): revert to --fail-on high and npm install esbuild@0.28.1
	# Suppressed: GHSA-gv7w-rqvm-qjhr (Deno-only RCE, N/A), GHSA-g7r4-m6w7-qqqr
	# (esbuild dev-server file read, N/A — build-time only, rust-embed).
	# Blocked by min-release-age=7 until 0.28.1 clears quarantine.
	cd $(FRONTEND_DIR) && npx cve-lite-cli . --verbose --fail-on critical
	@echo ""
	@echo "── npm audit (advisory check) ──────────────────────────────────────"
	cd $(FRONTEND_DIR) && npm audit --omit=dev || true
	@echo ""
	@echo "── lockfile integrity ──────────────────────────────────────────────"
	cd $(FRONTEND_DIR) && npx lockfile-lint \
		--path package-lock.json \
		--type npm \
		--allowed-hosts npm \
		--validate-https \
		--validate-package-names
	@echo ""
	@echo "  ✓ Supply chain checks passed"

# ── CI gate (run before pushing) ──────────────────────────────────────────────

ci: check check-frontend test audit
	@echo "  ✓ All checks passed"

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf $(FRONTEND_DIR)/node_modules $(FRONTEND_DIR)/dist
