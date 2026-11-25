#!/bin/sh
# The real lane never draws a pane, so a host without zig builds without the Ghostty engine.
set -eu
cd "$(dirname "$0")/../.."
if [ -x /opt/homebrew/opt/zig@0.15/bin/zig ] || command -v zig >/dev/null; then
  exec .agents/skills/verify-nebula/vn build
fi
CARGO_TARGET_DIR=target/verify-nebula exec cargo build -p nebula --no-default-features
