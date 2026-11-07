.PHONY: check fmt fmt-fix clippy test doc build release inspect submodule

PLUGIN_PATH := $(CURDIR)/target/debug

## Run all checks (fmt, clippy, tests, docs)
check: fmt clippy test doc

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

build:
	cargo build --workspace

release:
	cargo build --workspace --release

## Show what GStreamer sees of the freshly built plugins
inspect: build
	GST_PLUGIN_PATH=$(PLUGIN_PATH) gst-inspect-1.0 snapsink
	GST_PLUGIN_PATH=$(PLUGIN_PATH) gst-inspect-1.0 snapsrc

## Fetch / update the snapcast-rs submodule to the recorded commit
submodule:
	git submodule update --init --recursive
