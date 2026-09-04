# Dev helpers. `make install` puts the latest release build into ~/.cargo/bin.
# (End users install via install.sh; this is for working from a checkout.)

PREFIX ?= $(HOME)/.cargo/bin
BIN := target/release/nebula

.PHONY: build install kill build-novt install-novt

# The default build includes the Ghostty pane engine, whose libghostty-vt is
# compiled from ghostty sources by zig 0.15.2 (`brew install zig@0.15`); the
# keg is not linked, so put it on PATH here. The first build also git-clones
# those sources, so it is slow and needs network. `make build-novt` skips all
# of that and falls back to the vt100 + tui-term pane.
ZIG_0_15 := /opt/homebrew/opt/zig@0.15/bin
export PATH := $(ZIG_0_15):$(PATH)

build:
	cargo build --release

build-novt:
	cargo build --release -p nebula --no-default-features

# The cp+mv two-step is load-bearing on macOS: overwriting the installed
# binary in place reuses its inode, and the kernel's cached code signature
# for that inode no longer matches the new contents — every exec then dies
# with SIGKILL (exit 137). A fresh inode forces signature re-validation.
install: build
	cp $(BIN) $(PREFIX)/nebula.new
	mv $(PREFIX)/nebula.new $(PREFIX)/nebula
	@$(PREFIX)/nebula --version
	@$(PREFIX)/nebula _stale-daemon-note

install-novt: build-novt
	cp $(BIN) $(PREFIX)/nebula.new
	mv $(PREFIX)/nebula.new $(PREFIX)/nebula
	@$(PREFIX)/nebula --version

# Stops every active session — run only when you're ready to cut over.
kill:
	$(PREFIX)/nebula kill
