SHELL := /bin/bash

ROOT := $(CURDIR)
SWIFT_DIR := $(ROOT)/swift
GENERATED_DIR := $(SWIFT_DIR)/Generated
ARTIFACTS_DIR := $(ROOT)/artifacts
RUST_DYLIB := $(ROOT)/target/release/libfile_peeker_client.dylib
RUST_STATICLIB := $(ROOT)/target/release/libfile_peeker_client.a

.PHONY: all check fmt cargo-check clippy rust-test local-server-test runnable-test rust-library server-resource bindings client-integration-test xcode-project xcode-build verify clean-generated

all: check

check: fmt cargo-check clippy rust-test

fmt:
	cargo fmt --all -- --check

cargo-check:
	cargo check --workspace --all-targets

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

rust-test:
	cargo test --workspace

local-server-test: ; scripts/test-local-client-server.sh

runnable-test: ; scripts/test-local-tui.sh

rust-library:
	cargo build -p file-peeker-client --release
	test -f "$(RUST_DYLIB)"
	test -f "$(RUST_STATICLIB)"

bindings: rust-library
	mkdir -p "$(GENERATED_DIR)"
	cargo run -p uniffi-bindgen -- generate "$(RUST_DYLIB)" --language swift --out-dir "$(GENERATED_DIR)" --metadata-no-deps
	test -f "$(GENERATED_DIR)/FilePeekerClient.swift"
	test -f "$(GENERATED_DIR)/FilePeekerClientFFI.h"
	test -f "$(GENERATED_DIR)/FilePeekerClientFFI.modulemap"

client-integration-test: bindings server-resource
	mkdir -p "$(ARTIFACTS_DIR)"
	swiftc -parse-as-library \
		-module-name FilePeekerClientIntegrationTests \
		-module-cache-path "$(ARTIFACTS_DIR)/ModuleCache" \
		-I "$(GENERATED_DIR)" \
		-Xcc -fmodule-map-file="$(GENERATED_DIR)/FilePeekerClientFFI.modulemap" \
		-L "$(ROOT)/target/release" \
		-lfile_peeker_client \
		"$(GENERATED_DIR)/FilePeekerClient.swift" \
		"$(SWIFT_DIR)/ClientIntegrationTests/main.swift" \
		-o "$(ARTIFACTS_DIR)/file-peeker-client-integration-tests"
	DYLD_LIBRARY_PATH="$(ROOT)/target/release" "$(ARTIFACTS_DIR)/file-peeker-client-integration-tests"

server-resource: ; scripts/bundle-swift-server.sh

xcode-project: bindings server-resource
	cd "$(SWIFT_DIR)" && xcodegen generate

xcode-build: xcode-project
	xcodebuild \
		-project "$(SWIFT_DIR)/FilePeeker.xcodeproj" \
		-scheme FilePeeker \
		-configuration Debug \
		-derivedDataPath "$(SWIFT_DIR)/DerivedData" \
		CODE_SIGNING_ALLOWED=NO \
		build

verify: check local-server-test runnable-test client-integration-test xcode-build

clean-generated: ; rm -rf "$(GENERATED_DIR)" "$(ARTIFACTS_DIR)" "$(SWIFT_DIR)/Resources" "$(SWIFT_DIR)/FilePeeker.xcodeproj" "$(SWIFT_DIR)/DerivedData"
