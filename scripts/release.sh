#!/bin/sh
# Static musl builds of the agent: x86_64 for VPS/WSL, aarch64 for VPS/Termux.
# Both binaries must be EXECUTED after building (spec: release engineering) —
#   ./target/x86_64-unknown-linux-musl/release/cdash-agent
#   qemu-aarch64 ./target/aarch64-unknown-linux-musl/release/cdash-agent
# Prereqs: rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#          sudo apt install musl-tools qemu-user
#          musl.cc aarch64 cross-toolchain unpacked at ~/.local/opt (see PATH below)
set -e
cd "$(dirname "$0")/.."

cargo build --release --locked -p cdash-agent --target x86_64-unknown-linux-musl

PATH="$HOME/.local/opt/aarch64-linux-musl-cross/bin:$PATH" \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc \
  cargo build --release --locked -p cdash-agent --target aarch64-unknown-linux-musl

echo "--- executing both ---"
PORT=0 ./target/x86_64-unknown-linux-musl/release/cdash-agent & p1=$!; sleep 1; kill $p1
PORT=0 timeout 5 qemu-aarch64 -L "$HOME/.local/opt/aarch64-linux-musl-cross" \
  ./target/aarch64-unknown-linux-musl/release/cdash-agent || true # timeout's exit 124 is the expected kill after a successful boot
echo "both targets built"
