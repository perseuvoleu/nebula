#!/bin/sh
# The real lane never draws a pane, so a host without zig 0.15 builds without the Ghostty engine.
set -eu
cd "$(dirname "$0")/../.."
CARGO_TARGET_DIR=target/verify-nebula exec .agents/checks/cargo build -p nebula
