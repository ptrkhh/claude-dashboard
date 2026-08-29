#!/bin/sh
# Every local artifact:
#   x86_64-unknown-linux-musl   cdash-agent      VPS / WSL
#   aarch64-unknown-linux-musl  cdash-agent      VPS / Termux (Android)
#   x86_64-pc-windows-msvc      cdash-tauri.exe  Windows desktop client (x64)
#   aarch64-pc-windows-msvc     cdash-tauri.exe  Windows desktop client (ARM64)
#   aarch64-linux-android       *.apk            Android thin client (Termux)
#
# The two Linux binaries must be EXECUTED after building (spec: release
# engineering). The Windows client cannot run here, so its gate is that it
# links at all — which is also the gate on the cfg(not(windows)) split that
# keeps the agent out of it.
#
# Prereqs:
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                     x86_64-pc-windows-msvc aarch64-pc-windows-msvc
#   pip install ziglang && cargo install cargo-zigbuild   # musl libc + a cross
#       C compiler for aws-lc-sys, replacing musl-tools and the musl.cc
#       toolchain that stopped responding
#   cargo install cargo-xwin && apt install clang lld     # MSVC headers and
#       libs, fetched on first build
#   apt install qemu-user-static                          # runs the aarch64 gate
#   cargo install tauri-cli --version "^2"                # the APK build; the
#       npm CLI templates `node tauri` into gradle, which only resolves in an
#       npm-layout project — this is a Rust workspace
#   Android SDK + NDK, with ANDROID_HOME and NDK_HOME set
set -e
cd "$(dirname "$0")/.."

cargo zigbuild --release --locked -p cdash-agent --target x86_64-unknown-linux-musl
cargo zigbuild --release --locked -p cdash-agent --target aarch64-unknown-linux-musl
# Two Windows clients, because Snapdragon machines run Windows natively and an
# x64 build only gets there through Prism emulation. Each bundles the agent for
# its own architecture: the client is on the host, but WSL is native to the
# host, so an ARM64 host means an ARM64 distro. Pick the wrong installer and the
# copied agent will not exec in the distro — the reason lands in
# ~/cdash-agent.log, not at copy time.
#
# CDASH_AGENT_BIN embeds it; the setup screen writes it out for WSL to copy
# from /mnt/c. Same mechanism as the APK, different binary.
CDASH_AGENT_BIN="$PWD/target/x86_64-unknown-linux-musl/release/cdash-agent" \
  cargo xwin build --release --locked -p cdash-tauri --target x86_64-pc-windows-msvc
CDASH_AGENT_BIN="$PWD/target/aarch64-unknown-linux-musl/release/cdash-agent" \
  cargo xwin build --release --locked -p cdash-tauri --target aarch64-pc-windows-msvc

# The APK, only where the Android toolchain is set up — everything above builds
# without it. `android init` runs every time because gen/android is generated,
# not tracked: it is a build artifact of tauri.conf.json.
#
# CDASH_AGENT_BIN embeds the aarch64 agent built above; the app's setup screen
# hands it to Termux over loopback.
if [ -n "${ANDROID_HOME:-}" ] && [ -n "${NDK_HOME:-}" ]; then
  AGENT_BIN="$PWD/target/aarch64-unknown-linux-musl/release/cdash-agent"
  (
    cd crates/tauri-app
    cargo tauri android init

    # Gradle enables cleartext HTTP for debug only, and reaching the agent at
    # http://localhost:23274 is this client's entire job — a release APK without
    # this cannot talk to Termux at all.
    sed -i '/getByName("release")/a\        manifestPlaceholders["usesCleartextTraffic"] = "true"' \
      gen/android/app/build.gradle.kts

    # Release, not debug: the debug APK carries an unstripped 137 MB .so, which
    # is 138 MB of download for a phone. Release is 21 MB.
    CDASH_AGENT_BIN="$AGENT_BIN" cargo tauri android build --apk --target aarch64

    # Android refuses to install an unsigned APK at all, so "unsigned" is not a
    # shippable state. The SDK debug key makes it installable; it is a local
    # test signature, not a distribution one.
    BT="$ANDROID_HOME/build-tools/35.0.0"
    KS="$HOME/.android/debug.keystore"
    [ -f "$KS" ] || keytool -genkeypair -dname "CN=Android Debug,O=Android,C=US" \
      -alias androiddebugkey -keypass android -keystore "$KS" -storepass android \
      -validity 10000 -keyalg RSA -keysize 2048
    OUT=gen/android/app/build/outputs/apk/universal/release
    "$BT/zipalign" -p -f 4 "$OUT/app-universal-release-unsigned.apk" \
      "$OUT/cdash-dashboard-android-arm64.apk"
    "$BT/apksigner" sign --ks "$KS" --ks-pass pass:android \
      --ks-key-alias androiddebugkey --key-pass pass:android \
      "$OUT/cdash-dashboard-android-arm64.apk"
  )
else
  echo "skipping the APK: set ANDROID_HOME and NDK_HOME to build it" >&2
fi

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
test -s ./target/aarch64-pc-windows-msvc/release/cdash-tauri.exe
echo "both agents booted; both windows clients linked"

APK=crates/tauri-app/gen/android/app/build/outputs/apk/universal/release/cdash-dashboard-android-arm64.apk
[ -f "$APK" ] && echo "apk: $APK"
