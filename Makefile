SHELL := /bin/bash

.PHONY: all check docs-check fmt lint test verify

all: check

check: fmt lint test

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets

test:
	cargo test --workspace

docs-check:
	bash scripts/check-docs.sh
	git diff --check -- README.md docs scripts/check-docs.sh

verify: docs-check check
