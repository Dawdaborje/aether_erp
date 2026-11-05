.DEFAULT_GOAL := build

# One target dir for every ERP plugin, so the SDK and its dependencies compile once.
export CARGO_TARGET_DIR ?= $(abspath ../target)
WASM := $(CARGO_TARGET_DIR)/wasm32-unknown-unknown

# Release build (LTO, smallest wasm): use for shipping.
build:
	cargo build --release --target wasm32-unknown-unknown
	mkdir -p out
	cp $(WASM)/release/erp_pricing.wasm out/plugin.wasm

# Quick build while iterating: no LTO, parallel codegen.
fast:
	cargo build --profile fast --target wasm32-unknown-unknown
	mkdir -p out
	cp $(WASM)/fast/erp_pricing.wasm out/plugin.wasm

clean:
	rm -rf out

.PHONY: build fast clean
