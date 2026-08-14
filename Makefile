SHELL := /bin/bash

.PHONY: all check docs-check fmt lint slim-server test verify

all: check

check: fmt lint slim-server test

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets
# The desktop front end is outside `default`, so it needs naming to be linted.
	cargo clippy --features gui --all-targets

# Targets whose `required-features` are unmet are silently skipped unless named
# explicitly, so the slim build the remote installer uses needs its own check.
# It is also what keeps `client` out of a server-only build now that `client`
# pulls the server in: nothing here may reference `client`.
slim-server:
	cargo check --no-default-features --features server --bin file-peeker-server

test:
	cargo test
# The desktop front end is outside `default`, so its tests need naming too. They
# cover what is left once the shared model is in `browser`: the expansion policy
# a click goes through, and what the chrome derives from a path.
	cargo test --features gui gui::

docs-check:
	bash scripts/check-docs.sh
	git diff --check -- README.md docs scripts/check-docs.sh

verify: docs-check check
