.PHONY: ci test

VERSION := 1.0
ci: check test
	@echo "all green"

test: build
	cargo test

check-format:
	cargo fmt --check
