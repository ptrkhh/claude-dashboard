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

# A binary that dies instantly must not pass. `timeout`'s 124 IS the pass
# here — it means the agent served until killed — so the gate is the boot
# banner, not the exit status.
boots() {
  out=$(PORT=0 timeout 5 "$@" 2>&1 || true)
  echo "$out" | grep -q "cdash-agent .* on http://127.0.0.1:" || {
    echo "FAILED to boot: $*" >&2; echo "$out" >&2; exit 1
  }
}

echo "--- executing both ---"
boots ./target/x86_64-unknown-linux-musl/release/cdash-agent
boots qemu-aarch64 -L "$HOME/.local/opt/aarch64-linux-musl-cross" \
  ./target/aarch64-unknown-linux-musl/release/cdash-agent
echo "both targets built and booted"
