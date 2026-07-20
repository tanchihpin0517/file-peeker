SHELL := /bin/bash

ROOT := $(CURDIR)
SWIFT_DIR := $(ROOT)/swift
GENERATED_DIR := $(SWIFT_DIR)/Generated
RUST_DYLIB := $(ROOT)/target/release/libfile_peeker_client.dylib
RUST_STATICLIB := $(ROOT)/target/release/libfile_peeker_client.a

.PHONY: all check rust-library bindings xcode-project xcode-build verify clean-generated

all: check

check: xcode-build

rust-library:
	cargo build -p file-peeker-client --release
	test -f "$(RUST_DYLIB)"
	test -f "$(RUST_STATICLIB)"

bindings: rust-library
	mkdir -p "$(GENERATED_DIR)"
	cargo run -p uniffi-bindgen -- generate \
		--library "$(RUST_DYLIB)" \
		--language swift \
		--out-dir "$(GENERATED_DIR)" \
		--config "$(ROOT)/crates/file-peeker-client/uniffi.toml"
	test -f "$(GENERATED_DIR)/FilePeekerClient.swift"
	test -f "$(GENERATED_DIR)/FilePeekerClientFFI.h"
	test -f "$(GENERATED_DIR)/FilePeekerClientFFI.modulemap"

xcode-project: bindings
	cd "$(SWIFT_DIR)" && xcodegen generate

xcode-build: xcode-project
	xcodebuild \
		-project "$(SWIFT_DIR)/FilePeeker.xcodeproj" \
		-scheme FilePeeker \
		-configuration Debug \
		-derivedDataPath "$(SWIFT_DIR)/DerivedData" \
		CODE_SIGNING_ALLOWED=NO \
		build

verify: check

clean-generated: ; rm -rf "$(GENERATED_DIR)" "$(SWIFT_DIR)/FilePeeker.xcodeproj" "$(SWIFT_DIR)/DerivedData"
