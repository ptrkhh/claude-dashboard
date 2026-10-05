#!/usr/bin/env bash
# Installs everything scripts/release.sh needs, into one directory, without root:
#
#   cross-compiling the agent   zig + cargo-zigbuild        (musl, x86_64 and aarch64)
#   the Windows client          LLVM (clang-cl, lld-link) + cargo-xwin + MSVC libs
#   the Android APK             JDK + Android SDK/NDK + the Rust Tauri CLI
#   running the aarch64 gate    qemu-user-static
#
# Idempotent: every step checks whether it is already done, so it is safe to
# re-run after an interruption (downloads resume) or to repair a damaged tree.
#
#   scripts/toolchain.sh --accept-licenses     install what is missing
#   scripts/toolchain.sh --check               report only, change nothing
#   scripts/toolchain.sh --accept-licenses --skip-android
#
# Where: $CDASH_TOOLCHAIN (default ~/.cdash-toolchain), about 8 GB, plus ~3 GB of
# downloads at first build for gradle. Versions are pinned in toolchain-env.sh.
# Needs a Linux x86_64 host (Ubuntu, Debian, WSL2) with rustup.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=toolchain-env.sh
. "$here/toolchain-env.sh"
TC=$CDASH_TOOLCHAIN

accept=0 check=0 android=1
for arg in "$@"; do
  case "$arg" in
    --accept-licenses) accept=1 ;;
    --check) check=1 ;;
    --skip-android) android=0 ;;
    -h|--help) sed -n '2,/^set -e/p' "$0" | sed '$d;s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

log() { printf '\n==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Resumable, retrying download; verified against a checksum when one is given.
fetch() { # url dest [sha256]
  local url=$1 dest=$2 sha=${3:-}
  if [ -f "$dest" ] && { [ -z "$sha" ] || echo "$sha  $dest" | sha256sum -c --status; }; then return 0; fi
  mkdir -p "$(dirname "$dest")"
  curl -fL --retry 8 --retry-delay 5 -C - -o "$dest.part" "$url"
  if [ -n "$sha" ] && ! echo "$sha  $dest.part" | sha256sum -c --status; then
    rm -f -- "${dest:?}.part"
    die "checksum mismatch for $url"
  fi
  mv "$dest.part" "$dest"
}

RUST_TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc aarch64-pc-windows-msvc)
[ "$android" = 1 ] && RUST_TARGETS+=(aarch64-linux-android)

# ---- "is it done" predicates: the install steps and --check share them ----

ok_rust_targets() {
  local have_targets t
  have_targets=$(rustup target list --installed 2>/dev/null) || return 1
  for t in "${RUST_TARGETS[@]}"; do grep -qx "$t" <<<"$have_targets" || return 1; done
}
cargo_tool_ok() { # binary version
  "$TC/cargo/bin/$1" --version 2>/dev/null | grep -qF "$2"
}
ok_cargo_tools() {
  cargo_tool_ok cargo-zigbuild "$CDASH_CARGO_ZIGBUILD_VERSION" &&
    cargo_tool_ok cargo-xwin "$CDASH_CARGO_XWIN_VERSION" &&
    { [ "$android" = 0 ] || cargo_tool_ok cargo-tauri "$CDASH_TAURI_CLI_VERSION"; }
}
ok_zig() { [ "$("$TC/zig/zig" version 2>/dev/null)" = "$CDASH_ZIG_VERSION" ]; }
ok_llvm() {
  local t
  for t in clang-cl lld-link llvm-lib llvm-rc; do [ -x "$TC/bin/$t" ] || return 1; done
  "$TC/bin/clang-cl" --version >/dev/null 2>&1
}
ok_jdk() { [ "$android" = 0 ] || { [ -x "$TC/bin/java" ] && [ -x "$TC/bin/keytool" ] && "$TC/bin/java" -version >/dev/null 2>&1; }; }
ok_qemu() { have qemu-aarch64-static; }
ok_xwin() { [ -f "$XDG_CACHE_HOME/.xwin-warmed" ]; }
ok_android() {
  [ "$android" = 0 ] && return 0
  [ -x "$TC/android/cmdline-tools/latest/bin/sdkmanager" ] &&
    [ -d "$TC/android/ndk/$CDASH_ANDROID_NDK_VERSION" ] &&
    [ -d "$TC/android/platforms/${CDASH_ANDROID_PLATFORM#platforms;}" ] &&
    [ -d "$TC/android/build-tools/$CDASH_ANDROID_BUILD_TOOLS_VERSION" ] &&
    [ -x "$TC/android/platform-tools/adb" ]
}

COMPONENTS=(rust_targets cargo_tools zig llvm jdk qemu xwin android)

report() { # prints one line per component; returns 1 if any is missing
  local c bad=0
  for c in "${COMPONENTS[@]}"; do
    if "ok_$c"; then printf '  ok       %s\n' "$c"; else printf '  MISSING  %s\n' "$c"; bad=1; fi
  done
  return $bad
}

if [ "$check" = 1 ]; then
  echo "toolchain in $TC:"
  report && exit 0
  echo "run: scripts/toolchain.sh --accept-licenses" >&2
  exit 1
fi

# ---- preflight ----

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] ||
  die "this needs a Linux x86_64 host (Ubuntu, Debian or WSL2); this is $(uname -s) $(uname -m)"

missing=()
for t in curl tar xz unzip git cc make rustup sha256sum; do have "$t" || missing+=("$t"); done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "missing host tools: ${missing[*]}" >&2
  have apt-get && echo "  sudo apt install -y build-essential pkg-config libssl-dev curl unzip xz-utils git   # and rustup from https://rustup.rs" >&2
  exit 1
fi

# The two licenses this pulls in are accepted by whoever runs it, not by the script.
if [ "$accept" != 1 ]; then
  cat >&2 <<EOF
This installs software under licenses you must accept yourself:
  - Microsoft Visual C++ CRT and Windows SDK (fetched by cargo-xwin to build the
    Windows client): https://go.microsoft.com/fwlink/?LinkId=2086102
EOF
  [ "$android" = 1 ] && echo "  - Android SDK, platform-tools, build-tools and NDK: https://developer.android.com/studio/terms" >&2
  echo "Re-run with --accept-licenses to accept them and continue." >&2
  exit 2
fi

avail_gb=$(df -Pk "$(dirname "$TC")" 2>/dev/null | awk 'NR==2{print int($4/1048576)}')
if [ -n "${avail_gb:-}" ] && [ "$avail_gb" -lt 12 ]; then
  die "only ${avail_gb} GB free near $TC; the toolchain needs about 8 GB and a build 4 GB more"
fi

mkdir -p "$TC"

# ---- steps ----

install_rust_targets() { rustup target add "${RUST_TARGETS[@]}"; }

install_cargo_tools() {
  # cargo-zigbuild and cargo-xwin are small; tauri-cli takes a few minutes to compile.
  cargo_tool_ok cargo-zigbuild "$CDASH_CARGO_ZIGBUILD_VERSION" ||
    cargo install --locked --root "$TC/cargo" cargo-zigbuild --version "=$CDASH_CARGO_ZIGBUILD_VERSION"
  cargo_tool_ok cargo-xwin "$CDASH_CARGO_XWIN_VERSION" ||
    cargo install --locked --root "$TC/cargo" cargo-xwin --version "=$CDASH_CARGO_XWIN_VERSION"
  [ "$android" = 0 ] || cargo_tool_ok cargo-tauri "$CDASH_TAURI_CLI_VERSION" ||
    cargo install --locked --root "$TC/cargo" tauri-cli --version "=$CDASH_TAURI_CLI_VERSION"
}

install_zig() {
  local tgz="$TC/downloads/zig-$CDASH_ZIG_VERSION.tar.xz"
  fetch "https://ziglang.org/download/$CDASH_ZIG_VERSION/zig-x86_64-linux-$CDASH_ZIG_VERSION.tar.xz" "$tgz" "$CDASH_ZIG_SHA256"
  mkdir -p "$TC/zig"
  tar -xJf "$tgz" -C "$TC/zig" --strip-components=1
}

# LLVM and the JDK come from conda-forge through micromamba: one static binary,
# no python, no root, and the same builds on every machine.
install_llvm() {
  local mm="$TC/tools/micromamba" specs verb link
  fetch "https://github.com/mamba-org/micromamba-releases/releases/latest/download/micromamba-linux-64" "$mm"
  chmod +x "$mm"
  specs=("clang=$CDASH_LLVM_SPEC" "lld=$CDASH_LLVM_SPEC" "llvm-tools=$CDASH_LLVM_SPEC")
  [ "$android" = 0 ] || specs+=("openjdk=$CDASH_JDK_SPEC")
  verb=create
  [ -d "$TC/conda/conda-meta" ] && verb=install
  MAMBA_ROOT_PREFIX="$TC/mamba" "$mm" "$verb" -y -p "$TC/conda" -c conda-forge --override-channels "${specs[@]}"
  # Expose exactly what the build asks for; see toolchain-env.sh.
  mkdir -p "$TC/bin"
  for link in clang-cl lld-link llvm-lib llvm-rc llvm-ar llvm-dlltool; do
    ln -sf "$TC/conda/bin/$link" "$TC/bin/$link"
  done
  if [ "$android" = 1 ]; then
    for link in java keytool; do ln -sf "$TC/conda/bin/$link" "$TC/bin/$link"; done
  fi
}

# The JDK comes from the same environment, so its step is the same step: it adds
# openjdk to a tree that has LLVM already.
install_jdk() { install_llvm; }

install_qemu() {
  # `apt-get download` fetches a .deb without root; qemu-user-static is a static
  # binary, so unpacking it into place is all it takes.
  have apt-get || die "qemu-aarch64-static is missing and this is not an apt system: install qemu-user-static with your package manager"
  mkdir -p "$TC/qemu"
  ( cd "$TC/qemu" && apt-get download qemu-user-static && dpkg-deb -x ./qemu-user-static_*.deb . )
}

# Pre-fetch the MSVC CRT and Windows SDK (~1 GB) here, with retries, rather than
# in the middle of the first build.
install_xwin() {
  local target
  for target in x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
    cargo xwin env --target "$target" >/dev/null
  done
  mkdir -p "$XDG_CACHE_HOME" && touch "$XDG_CACHE_HOME/.xwin-warmed"
}

# sdkmanager has no timeout, and a connection that dies mid-download leaves it
# waiting forever (seen at 45% of the NDK, with the network fine). So watch the
# SDK directory: if it has not grown for $stall seconds, kill sdkmanager and run
# it again. It cannot resume, so this restarts that package, but it finishes.
# Uses $sdk and $sm from install_android.
sdk_install() {
  local stall=240 attempt pid last size idle
  for attempt in 1 2 3 4; do
    "${sm[@]}" --install "$@" &
    pid=$!
    last=-1 idle=0
    while kill -0 "$pid" 2>/dev/null; do
      sleep 10
      size=$(du -sk "$sdk" 2>/dev/null | cut -f1)
      if [ "$size" = "$last" ]; then idle=$((idle + 10)); else idle=0 last=$size; fi
      if [ "$idle" -ge "$stall" ]; then
        echo "sdkmanager made no progress for ${stall}s; restarting (attempt $attempt of 4)" >&2
        kill "$pid" 2>/dev/null || true
        break
      fi
    done
    if wait "$pid"; then return 0; fi
  done
  return 1
}

install_android() {
  [ "$android" = 0 ] && return 0
  local sdk="$TC/android" zip="$TC/downloads/android-cmdline-tools-$CDASH_ANDROID_CMDLINE_BUILD.zip" tmp sm
  fetch "https://dl.google.com/android/repository/commandlinetools-linux-${CDASH_ANDROID_CMDLINE_BUILD}_latest.zip" "$zip" "$CDASH_ANDROID_CMDLINE_SHA256"
  if [ ! -x "$sdk/cmdline-tools/latest/bin/sdkmanager" ]; then
    mkdir -p "$sdk/cmdline-tools"
    tmp=$(mktemp -d -p "$sdk")
    unzip -q "$zip" -d "$tmp"
    [ ! -e "$sdk/cmdline-tools/latest" ] || mv "$sdk/cmdline-tools/latest" "$sdk/cmdline-tools/latest.broken.$$"
    mv "$tmp/cmdline-tools" "$sdk/cmdline-tools/latest"
    rmdir "$tmp"
  fi
  sm=("$sdk/cmdline-tools/latest/bin/sdkmanager" "--sdk_root=$sdk")
  # --accept-licenses was required above; this is where it takes effect.
  { yes || true; } | "${sm[@]}" --licenses >/dev/null
  sdk_install "platform-tools" "$CDASH_ANDROID_PLATFORM" \
    "build-tools;$CDASH_ANDROID_BUILD_TOOLS_VERSION" "ndk;$CDASH_ANDROID_NDK_VERSION"
}

for c in "${COMPONENTS[@]}"; do
  if "ok_$c"; then
    log "$c: already installed"
  else
    log "$c: installing"
    "install_$c"
  fi
done

log "verifying"
if report; then
  cat <<EOF

Toolchain ready in $TC ($(du -sh "$TC" | cut -f1)).
Build everything:   scripts/build-all.sh
Build by hand:      . scripts/toolchain-env.sh && sh scripts/release.sh
The first APK build also downloads gradle and Android platform packages into
$TC/gradle and $TC/android (a few hundred MB).
EOF
else
  die "something above is still missing; re-run to retry"
fi
