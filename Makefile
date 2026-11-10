.PHONY: check fmt fmt-fix clippy test doc build release inspect submodule \
	install uninstall install-system uninstall-system test-interop doctor \
	listen-send listen-recv listen-cpp-client listen-cpp-server listen-pair listen-all

PLUGIN_PATH := $(CURDIR)/target/debug
PLUGINS := libgstsnapsink.so libgstsnapsrc.so
RELEASE_DIR := $(CURDIR)/target/release

# Per-user GStreamer plugin directory (scanned automatically, no root needed).
PLUGIN_DIR ?= $(HOME)/.local/share/gstreamer-1.0/plugins
# System plugin directory as reported by GStreamer itself
# (Fedora: /usr/lib64/gstreamer-1.0, Debian: /usr/lib/<triplet>/gstreamer-1.0).
SYSTEM_PLUGIN_DIR ?= $(shell pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null)

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

## Install the release plugins for the current user (into PLUGIN_DIR)
install: release
	install -d $(PLUGIN_DIR)
	install -m 0755 $(addprefix $(RELEASE_DIR)/,$(PLUGINS)) $(PLUGIN_DIR)/
	@echo "Installed into $(PLUGIN_DIR). Check with: make doctor"

uninstall:
	rm -f $(addprefix $(PLUGIN_DIR)/,$(PLUGINS))

## System-wide install. Build as yourself first: make release && sudo make install-system
install-system:
	@test -n "$(SYSTEM_PLUGIN_DIR)" || { echo "cannot find the GStreamer plugins dir (pkg-config)"; exit 1; }
	@for f in $(PLUGINS); do test -f $(RELEASE_DIR)/$$f || { echo "$$f missing: run 'make release' first (not as root)"; exit 1; }; done
	install -m 0755 $(addprefix $(RELEASE_DIR)/,$(PLUGINS)) $(SYSTEM_PLUGIN_DIR)/

uninstall-system:
	@test -n "$(SYSTEM_PLUGIN_DIR)" || { echo "cannot find the GStreamer plugins dir (pkg-config)"; exit 1; }
	rm -f $(addprefix $(SYSTEM_PLUGIN_DIR)/,$(PLUGINS))

## Interop tests against the reference C++ snapclient/snapserver
## (override the binaries with SNAPCLIENT=... SNAPSERVER=...)
test-interop:
	cargo test -p gst-plugin-snapsink --test interop_cpp -- --ignored
	cargo test -p gst-plugin-snapsrc --test interop_cpp -- --ignored

## Report tools, elements and sound devices needed for building and testing
doctor:
	@scripts/doctor.sh $(PLUGIN_DIR)

## Listening tests (see scripts/listen.sh for SRC, HOST, PORT, CODEC, BUFFER_MS, ...).
## They use the installed plugins; LOCAL=1 uses a fresh release build from this tree instead.
ifeq ($(LOCAL),1)
LISTEN_ENV := GST_PLUGIN_PATH=$(RELEASE_DIR)
LISTEN_DEPS := release
endif

listen-send listen-recv listen-cpp-client listen-cpp-server listen-pair listen-all: $(LISTEN_DEPS)
	$(LISTEN_ENV) scripts/listen.sh $(patsubst listen-%,%,$@)
