SHELL := /bin/bash

# The shipping crate's own checks, run from this directory. The documentation and
# shell checks are repository-wide and live in the root Makefile, which also runs
# these as `make old-check`.

# Locally a clippy finding stays a warning, so a half-finished edit still
# compiles and runs. CI passes `LINT_DENY=-Dwarnings` to turn the same findings
# into a failure; the clippy invocation itself is identical either way.
LINT_DENY ?=

.PHONY: all check fmt lint slim-server test

all: check

check: fmt lint slim-server test

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets -- $(LINT_DENY)
# The desktop front end is outside `default`, so it needs naming to be linted.
	cargo clippy --features gui --all-targets -- $(LINT_DENY)

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
