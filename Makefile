.PHONY: setup check fmt fmt-fix clippy test doc build release inspect submodule \
	install uninstall install-system uninstall-system test-interop test-aloop test-endurance test-mdns doctor \
	listen-send listen-recv listen-cpp-client listen-cpp-server listen-pair listen-all listen-show

PLUGIN_PATH := $(CURDIR)/target/debug
PLUGINS := libgstsnapsink.so libgstsnapsrc.so
RELEASE_DIR := $(CURDIR)/target/release

# Per-user GStreamer plugin directory (scanned automatically, no root needed).
PLUGIN_DIR ?= $(HOME)/.local/share/gstreamer-1.0/plugins
# System plugin directory as reported by GStreamer itself
# (Fedora: /usr/lib64/gstreamer-1.0, Debian: /usr/lib/<triplet>/gstreamer-1.0).
SYSTEM_PLUGIN_DIR ?= $(shell pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null)

## First-time setup: git hooks here and in the snapcast-rs submodule (.githooks:
## Conventional Commits, fmt on commit, `make check` on push)
setup:
	git config core.hooksPath .githooks
	$(MAKE) -C snapcast-rs setup
	@echo "Git hooks configured (this repo and snapcast-rs)"

## Run all checks (fmt, clippy, tests, docs)
check: fmt clippy test doc

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	# snapsink as shipped (no control API) and without the daemon client (Android)
	cargo clippy -p gst-plugin-snapsink --lib -- -D warnings
	cargo clippy -p gst-plugin-snapsink --lib --no-default-features -- -D warnings

test:
	cargo test --workspace

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

build:
	cargo build --workspace

# Each crate on its own: in one workspace build Cargo would merge the features of
# all crates, and snapsink would get the daemon's control API (HTTP, auth).
# SNAPSINK_FEATURES: extra snapsink features. `control` (the API in embedded mode)
# stays the default until `make install` installs and autospawns the daemon
# (docs/shared-server.md, steps 3 and 5); `make release SNAPSINK_FEATURES=` is the lean build.
SNAPSINK_FEATURES ?= control
release:
	cargo build --release -p gst-plugin-snapsink $(if $(SNAPSINK_FEATURES),--features $(SNAPSINK_FEATURES))
	cargo build --release -p gst-plugin-snapsrc
	cargo build --release -p gst-snapcast-daemon

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

## Timing tests on the snd-aloop loopback card (F2): two receivers, offset and drift.
## Needs snd-aloop, amixer and (for the C++ cases) snapclient. One test at a time:
## they share loopback subdevice 0. See docs/testing.md.
test-aloop:
	cargo test -p gst-plugin-snapsrc --test aloop -- --ignored --test-threads=1 --nocapture \
		--skip aloop_endurance

## F3: snapsrc + C++ snapclient for ENDURANCE_MINUTES (default 60) on snd-aloop; one CSV
## row per window (target/endurance-*.csv). Same needs as test-aloop. See docs/testing.md.
test-endurance:
	cargo test -p gst-plugin-snapsrc --test aloop -- --ignored --exact aloop_endurance --nocapture

## mDNS: snapsrc without a host finds a snapsink (needs multicast on the network)
test-mdns:
	cargo test -p gst-plugin-snapsrc --test mdns -- --ignored

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

## Print the commands a listening test runs, with notes, without running them (WHAT=send|recv|...)
listen-show:
	@$(LISTEN_ENV) scripts/listen.sh show $(or $(WHAT),all)
