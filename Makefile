SHELL := /bin/bash

# Locally a clippy finding stays a warning, so a half-finished edit still
# compiles and runs. CI passes `LINT_DENY=-Dwarnings` to turn the same findings
# into a failure; the clippy invocation itself is identical either way.
LINT_DENY ?=

.PHONY: all check docs-check fmt lint old-check shell-check test verify

all: check

# v2, the crate at the repository root.
check: fmt lint test

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets -- $(LINT_DENY)

test:
	cargo test

# The shipping crate in `old/`, a workspace of its own with its own Makefile.
old-check:
	$(MAKE) -C old check LINT_DENY=$(LINT_DENY)

docs-check:
	bash scripts/check-docs.sh
	git diff --check -- README.md TODO.md docs old/README.md old/docs scripts/check-docs.sh

# The remote install path is shell that nothing else checks: `ensure-server.sh`
# runs over an SSH stdin pipe and `install-server-from-source.sh` runs on a host
# this project never sees. The file list comes from git so a script added later
# is covered without being named here, and so `run-tui.sh` and the documentation
# checker are covered too.
shell-check:
	shellcheck $$(git ls-files '*.sh')

verify: docs-check shell-check check old-check
