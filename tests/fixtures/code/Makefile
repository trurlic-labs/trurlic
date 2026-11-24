.PHONY: ci test

VERSION := 1.0
ci: check test
	@echo "all green"

test: build \
      lint
	cargo test

	# still the recipe
	cargo test --doc

define TEMPLATE
fake: rule
endef

check-format:
	cargo fmt --check
