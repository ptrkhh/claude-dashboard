#!/usr/bin/env bash
# Builds every local artifact with scripts/release.sh and collects the results,
# with checksums, into dist/ (override with CDASH_DIST):
#
#   cdash-agent-linux-x86_64          VPS, WSL (x64)
#   cdash-agent-linux-aarch64         VPS, Termux
#   cdash-tauri-windows-x64.exe       Windows desktop client, bundles the x86_64 agent
#   cdash-tauri-windows-arm64.exe     Windows on ARM client, bundles the aarch64 agent
#   cdash-dashboard-android-arm64.apk Android client, bundles the aarch64 agent
#
# Set the toolchain up once first:  scripts/toolchain.sh --accept-licenses
#
#   scripts/build-all.sh             everything
#   scripts/build-all.sh --no-apk    skip the APK (no Android toolchain needed)
set -euo pipefail
cd "$(dirname "$0")/.."

no_apk=0
case "${1:-}" in
  "") ;;
  --no-apk) no_apk=1 ;;
  *) echo "usage: scripts/build-all.sh [--no-apk]" >&2; exit 2 ;;
esac

# shellcheck source=toolchain-env.sh
. scripts/toolchain-env.sh

check=(--check)
[ "$no_apk" = 1 ] && check+=(--skip-android)
scripts/toolchain.sh "${check[@]}" || {
  echo "Set the toolchain up first:  scripts/toolchain.sh --accept-licenses" >&2
  exit 1
}

# release.sh builds the APK exactly when these are set; unsetting them is how it
# is told to skip.
if [ "$no_apk" = 1 ]; then unset ANDROID_HOME NDK_HOME; fi
# release.sh addresses ./target directly.
unset CARGO_TARGET_DIR
# Stops the Tauri CLI from asking questions nobody is there to answer.
export CI=true

sh scripts/release.sh

dist=${CDASH_DIST:-dist}
mkdir -p "$dist"
put() { cp "$1" "$dist/$2"; }
put target/x86_64-unknown-linux-musl/release/cdash-agent cdash-agent-linux-x86_64
put target/aarch64-unknown-linux-musl/release/cdash-agent cdash-agent-linux-aarch64
put target/x86_64-pc-windows-msvc/release/cdash-tauri.exe cdash-tauri-windows-x64.exe
put target/aarch64-pc-windows-msvc/release/cdash-tauri.exe cdash-tauri-windows-arm64.exe
apk=crates/tauri-app/gen/android/app/build/outputs/apk/universal/release/cdash-dashboard-android-arm64.apk
if [ "$no_apk" = 0 ]; then put "$apk" cdash-dashboard-android-arm64.apk; fi

# The one thing release.sh cannot see: that each client carries the agent for its
# OWN architecture. A mismatch builds, installs, and then fails in WSL or Termux
# with "Exec format error", far from here.
if command -v python3 >/dev/null 2>&1; then
  python3 - "$dist" "$no_apk" <<'PY'
import sys, zipfile
dist, no_apk = sys.argv[1], sys.argv[2] == "1"
def read(name): return open(f"{dist}/{name}", "rb").read()
pairs = [("cdash-tauri-windows-x64.exe", "cdash-agent-linux-x86_64", None),
         ("cdash-tauri-windows-arm64.exe", "cdash-agent-linux-aarch64", None)]
if not no_apk:
    pairs.append(("cdash-dashboard-android-arm64.apk", "cdash-agent-linux-aarch64", "lib/arm64-v8a/libcdash_tauri_lib.so"))
bad = 0
for client, agent, member in pairs:
    blob = zipfile.ZipFile(f"{dist}/{client}").read(member) if member else read(client)
    ok = read(agent) in blob
    print(f"  {'ok ' if ok else 'BAD'} {client} embeds {agent}")
    bad += not ok
sys.exit(bad)
PY
else
  echo "python3 not found: skipped the check that each client embeds its own architecture's agent" >&2
fi

{
  echo "commit:  $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- 2>/dev/null || echo ' (+uncommitted changes)')"
  echo "built:   $(date -Is)"
  echo "host:    $(uname -srm)"
  echo "rustc:   $(rustc --version)"
  echo "zig:     $CDASH_ZIG_VERSION   cargo-xwin: $CDASH_CARGO_XWIN_VERSION   tauri-cli: $CDASH_TAURI_CLI_VERSION"
} > "$dist/BUILD-INFO.txt"
(cd "$dist" && sha256sum cdash-* > SHA256SUMS.txt)

echo
echo "artifacts in $dist/:"
(cd "$dist" && ls -l cdash-* | awk '{printf "  %6.1f MB  %s\n", $5/1048576, $9}')
