.PHONY: setup check fmt fmt-fix clippy test doc build release inspect submodule \
	dist dlna-build dlna-session install uninstall install-system uninstall-system test-interop test-aloop test-aloop-shared test-endurance test-mdns doctor \
	listen-send listen-recv listen-cpp-client listen-cpp-server listen-pair listen-all listen-show

PLUGIN_PATH := $(CURDIR)/target/debug
PLUGINS := libgstsnapsink.so libgstsnapsrc.so
DAEMON := gst-snapcast-daemon
RELEASE_DIR := $(CURDIR)/target/release

# Per-user GStreamer plugin directory (scanned automatically, no root needed).
PLUGIN_DIR ?= $(HOME)/.local/share/gstreamer-1.0/plugins
# System plugin directory as reported by GStreamer itself
# (Fedora: /usr/lib64/gstreamer-1.0, Debian: /usr/lib/<triplet>/gstreamer-1.0).
SYSTEM_PLUGIN_DIR ?= $(shell pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null)
# Where the daemon goes: snapsink starts it from PATH (or GST_SNAPCAST_DAEMON).
BIN_DIR ?= $(HOME)/.local/bin
SYSTEM_BIN_DIR ?= /usr/local/bin

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
# SNAPSINK_FEATURES: extra snapsink features, e.g. `control` for the control API in
# mode embedded (in modes auto/shared the daemon serves it).
SNAPSINK_FEATURES ?=
release:
	cargo build --release -p gst-plugin-snapsink $(if $(SNAPSINK_FEATURES),--features $(SNAPSINK_FEATURES))
	cargo build --release -p gst-plugin-snapsrc
	cargo build --release -p gst-snapcast-daemon

## A tarball for users: plugins, daemon, install.sh, README.txt (target/dist/<name>.tar.gz).
## Unpack it and run ./install.sh (no root) or sudo ./install.sh --system.
DIST_VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST_COMMIT = $(shell git rev-parse --short HEAD)$(shell git diff --quiet HEAD -- || echo -dirty)
DIST_NAME = gstreamer-snapcast-rs-$(DIST_VERSION)-$(DIST_COMMIT)-linux-$(shell uname -m)
DIST_DIR := $(CURDIR)/target/dist
DIST_STAGE = $(DIST_DIR)/$(DIST_NAME)
dist: release
	install -d $(DIST_STAGE)/lib/gstreamer-1.0 $(DIST_STAGE)/bin
	install -m 0755 $(addprefix $(RELEASE_DIR)/,$(PLUGINS)) $(DIST_STAGE)/lib/gstreamer-1.0/
	install -m 0755 $(RELEASE_DIR)/$(DAEMON) $(DIST_STAGE)/bin/
	install -m 0755 scripts/dist-install.sh $(DIST_STAGE)/install.sh
	install -m 0755 scripts/fetch-snapweb.sh $(DIST_STAGE)/
	install -m 0644 scripts/dist-README.txt $(DIST_STAGE)/README.txt
	install -m 0644 LICENSE $(DIST_STAGE)/
	scripts/dist-build-info.sh "$(DIST_VERSION)" "$(DIST_COMMIT)" "$(SNAPSINK_FEATURES)" > $(DIST_STAGE)/BUILD-INFO
	tar -C $(DIST_DIR) -czf $(DIST_STAGE).tar.gz $(DIST_NAME)
	@echo "Packed $(DIST_STAGE).tar.gz"

## Show what GStreamer sees of the freshly built plugins
inspect: build
	GST_PLUGIN_PATH=$(PLUGIN_PATH) gst-inspect-1.0 snapsink
	GST_PLUGIN_PATH=$(PLUGIN_PATH) gst-inspect-1.0 snapsrc

## Fetch / update the snapcast-rs submodule to the recorded commit
submodule:
	git submodule update --init --recursive

## Install the release plugins (into PLUGIN_DIR) and the shared server daemon (into
## BIN_DIR, which must be on PATH for snapsink to start it) for the current user
install: release
	install -d $(PLUGIN_DIR) $(BIN_DIR)
	install -m 0755 $(addprefix $(RELEASE_DIR)/,$(PLUGINS)) $(PLUGIN_DIR)/
	install -m 0755 $(RELEASE_DIR)/$(DAEMON) $(BIN_DIR)/
	@echo "Installed into $(PLUGIN_DIR) and $(BIN_DIR). Check with: make doctor"
	@case ":$$PATH:" in *":$(BIN_DIR):"*) ;; *) echo "Note: $(BIN_DIR) is not on PATH, so snapsink cannot start $(DAEMON) (extend PATH or set GST_SNAPCAST_DAEMON)";; esac

uninstall:
	rm -f $(addprefix $(PLUGIN_DIR)/,$(PLUGINS)) $(BIN_DIR)/$(DAEMON)

## System-wide install. Build as yourself first: make release && sudo make install-system
install-system:
	@test -n "$(SYSTEM_PLUGIN_DIR)" || { echo "cannot find the GStreamer plugins dir (pkg-config)"; exit 1; }
	@for f in $(PLUGINS) $(DAEMON); do test -f $(RELEASE_DIR)/$$f || { echo "$$f missing: run 'make release' first (not as root)"; exit 1; }; done
	install -m 0755 $(addprefix $(RELEASE_DIR)/,$(PLUGINS)) $(SYSTEM_PLUGIN_DIR)/
	install -m 0755 $(RELEASE_DIR)/$(DAEMON) $(SYSTEM_BIN_DIR)/

uninstall-system:
	@test -n "$(SYSTEM_PLUGIN_DIR)" || { echo "cannot find the GStreamer plugins dir (pkg-config)"; exit 1; }
	rm -f $(addprefix $(SYSTEM_PLUGIN_DIR)/,$(PLUGINS)) $(SYSTEM_BIN_DIR)/$(DAEMON)

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

## The same timing tests with the sender in the shared server daemon (ALOOP_SENDER_MODE=shared):
## the daemon from this tree, autospawned with a runtime directory of its own
## (target/aloop-runtime; its daemon.log stays there for reading).
ALOOP_RUNTIME_DIR := $(CURDIR)/target/aloop-runtime
test-aloop-shared:
	cargo build -p gst-snapcast-daemon
	install -d -m 0700 $(ALOOP_RUNTIME_DIR)
	XDG_RUNTIME_DIR=$(ALOOP_RUNTIME_DIR) GST_SNAPCAST_DAEMON=$(PLUGIN_PATH)/$(DAEMON) \
	ALOOP_SENDER_MODE=shared \
		cargo test -p gst-plugin-snapsrc --test aloop -- --ignored --test-threads=1 --nocapture \
		--skip aloop_endurance

## F3: snapsrc + C++ snapclient for ENDURANCE_MINUTES (default 60) on snd-aloop; one CSV
## row per window (target/endurance-*.csv). Same needs as test-aloop. See docs/testing.md.
test-endurance:
	cargo test -p gst-plugin-snapsrc --test aloop -- --ignored --exact aloop_endurance --nocapture

## mDNS: snapsrc without a host finds a snapsink (needs multicast on the network)
test-mdns:
	cargo test -p gst-plugin-snapsrc --test mdns -- --ignored

## DLNA PoC (docs/dlna.md): build the dms and gmrender-resurrect forks, and dms's session
## feeder (snapsink's example session-sender), into target/dlna.
## Needs Go, libupnp (Debian libupnp-dev, Fedora libupnp-devel), autotools and the
## GStreamer development files.
DLNA_DIR := $(CURDIR)/target/dlna
dlna-build:
	cd dlna/dms && go build -o $(DLNA_DIR)/dms .
	@test ! -f dlna/gmrender-resurrect/config.status || { echo "dlna/gmrender-resurrect was configured in place (a build by hand there);" \
		"make dlna-build builds out of tree: run 'make distclean' in dlna/gmrender-resurrect first"; exit 1; }
	cd dlna/gmrender-resurrect && { test configure -nt configure.ac || ./autogen.sh; }
	install -d $(DLNA_DIR)/gmrender-build
	# Installed into target/dlna/gmrender (not run from the build tree): gmrender serves
	# its icons from <prefix>/share, and control points may skip a renderer whose icons fail.
	cd $(DLNA_DIR)/gmrender-build && \
		{ grep -qF -- '--prefix=$(DLNA_DIR)/gmrender' config.status 2>/dev/null || \
		  $(CURDIR)/dlna/gmrender-resurrect/configure --prefix=$(DLNA_DIR)/gmrender; } && \
		$(MAKE) && $(MAKE) install
	ln -sf gmrender/bin/gmediarender $(DLNA_DIR)/gmediarender
	# dms's session feeder (-snapcastFeeder): plays an item into a Snapcast stream.
	cargo build --release -p gst-plugin-snapsink --features control --example session-sender
	ln -sf $(RELEASE_DIR)/examples/session-sender $(DLNA_DIR)/session-sender
	@echo "Built $(DLNA_DIR)/dms, $(DLNA_DIR)/gmediarender and $(DLNA_DIR)/session-sender"

## DLNA PoC: a file as a Snapcast session that obeys play/pause/seek (stands in for dms
## until it feeds sessions): SESSION_FILE, STREAM (default), PORT (1704; control: PORT+1).
SESSION_FILE ?= assets/audio/freejazz_medley.flac
dlna-session:
	cargo run --release -p gst-plugin-snapsink --features control --example session-sender -- \
		"$(SESSION_FILE)" $(or $(STREAM),default) $(or $(PORT),1704)

## Report tools, elements and sound devices needed for building and testing
doctor:
	@scripts/doctor.sh $(PLUGIN_DIR)

## Listening tests (see scripts/listen.sh for SRC, HOST, PORT, CODEC, BUFFER_MS, ...).
## They use the installed plugins; LOCAL=1 uses a fresh release build from this tree instead.
ifeq ($(LOCAL),1)
LISTEN_ENV := GST_PLUGIN_PATH=$(RELEASE_DIR) GST_SNAPCAST_DAEMON=$(RELEASE_DIR)/$(DAEMON)
LISTEN_DEPS := release
endif

listen-send listen-recv listen-cpp-client listen-cpp-server listen-pair listen-all: $(LISTEN_DEPS)
	$(LISTEN_ENV) scripts/listen.sh $(patsubst listen-%,%,$@)

## Print the commands a listening test runs, with notes, without running them (WHAT=send|recv|...)
listen-show:
	@$(LISTEN_ENV) scripts/listen.sh show $(or $(WHAT),all)
