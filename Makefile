SHELL := /bin/bash

.PHONY: all check docs-check fmt lint slim-server test verify

all: check

check: fmt lint slim-server test

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets

# Targets whose `required-features` are unmet are silently skipped unless named
# explicitly, so the slim build the remote installer uses needs its own check.
# It is also what keeps `client` out of a server-only build now that `client`
# pulls the server in: nothing here may reference `session` or `client`.
slim-server:
	cargo check --no-default-features --features server --bin file-peeker-server

test:
	cargo test

docs-check:
	bash scripts/check-docs.sh
	git diff --check -- README.md docs scripts/check-docs.sh

verify: docs-check check
