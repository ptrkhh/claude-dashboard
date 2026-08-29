#!/bin/sh
# Every local artifact:
#   x86_64-unknown-linux-musl   cdash-agent      VPS / WSL
#   aarch64-unknown-linux-musl  cdash-agent      VPS / Termux (Android)
#   x86_64-pc-windows-msvc      cdash-tauri.exe  Windows desktop client
#
# The two Linux binaries must be EXECUTED after building (spec: release
# engineering). The Windows client cannot run here, so its gate is that it
# links at all — which is also the gate on the cfg(not(windows)) split that
# keeps the agent out of it.
#
# Prereqs:
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                     x86_64-pc-windows-msvc
#   pip install ziglang && cargo install cargo-zigbuild   # musl libc + a cross
#       C compiler for aws-lc-sys, replacing musl-tools and the musl.cc
#       toolchain that stopped responding
#   cargo install cargo-xwin && apt install clang lld     # MSVC headers and
#       libs, fetched on first build
#   apt install qemu-user-static                          # runs the aarch64 gate
set -e
cd "$(dirname "$0")/.."

cargo zigbuild --release --locked -p cdash-agent --target x86_64-unknown-linux-musl
cargo zigbuild --release --locked -p cdash-agent --target aarch64-unknown-linux-musl
cargo xwin build --release --locked -p cdash-tauri --target x86_64-pc-windows-msvc

# A binary that dies instantly must not pass. `timeout`'s 124 IS the pass
# here — it means the agent served until killed — so the gate is the boot
# banner, not the exit status.
boots() {
  out=$(PORT=0 timeout 5 "$@" 2>&1 || true)
  echo "$out" | grep -q "cdash-agent .* on http://127.0.0.1:" || {
    echo "FAILED to boot: $*" >&2; echo "$out" >&2; exit 1
  }
}

echo "--- executing both agents ---"
boots ./target/x86_64-unknown-linux-musl/release/cdash-agent
boots qemu-aarch64-static ./target/aarch64-unknown-linux-musl/release/cdash-agent

test -s ./target/x86_64-pc-windows-msvc/release/cdash-tauri.exe
echo "both agents booted; windows client linked"
