# shellcheck shell=bash
# The build toolchain's location and pinned versions. One file, two readers:
#
#   scripts/toolchain.sh   installs what this names
#   scripts/build-all.sh   sources it, then runs scripts/release.sh
#
# Source it yourself to build by hand in the current shell:
#
#   . scripts/toolchain-env.sh
#   sh scripts/release.sh
#
# Everything lives under one directory, so removing the toolchain is one `rm -rf`
# and two checkouts can share it. Override the location with CDASH_TOOLCHAIN.
# No root is needed anywhere.

CDASH_TOOLCHAIN="${CDASH_TOOLCHAIN:-$HOME/.cdash-toolchain}"

# ---- Pinned versions. Bump deliberately: each was built and booted together. ----

# musl libc and a cross C compiler for aws-lc-sys (cargo-zigbuild drives it).
CDASH_ZIG_VERSION=0.16.0
CDASH_ZIG_SHA256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
CDASH_CARGO_ZIGBUILD_VERSION=0.23.4
# MSVC headers and libs, fetched on first use, for the Windows client.
CDASH_CARGO_XWIN_VERSION=0.23.1
# The Rust Tauri CLI. The npm one templates `node tauri` into gradle, which only
# resolves in an npm-layout project; this is a Rust workspace.
CDASH_TAURI_CLI_VERSION=2.12.1
# clang-cl, lld-link and llvm-lib (cargo-xwin), a JDK (gradle, keytool).
CDASH_LLVM_SPEC=21.1
CDASH_JDK_SPEC=17
# Android: sdkmanager bootstrap, then the packages `cargo tauri android build` needs.
CDASH_ANDROID_CMDLINE_BUILD=11076708
CDASH_ANDROID_CMDLINE_SHA256=2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258
CDASH_ANDROID_PLATFORM="platforms;android-37.0"
CDASH_ANDROID_BUILD_TOOLS_VERSION=37.0.0
CDASH_ANDROID_NDK_VERSION=29.0.14206865

# ---- Paths and environment ----

_tc="$CDASH_TOOLCHAIN"

# Our tools first, but only ones we put there: $_tc/bin holds symlinks to exactly
# the LLVM and JDK binaries the build asks for, so nothing else in the conda
# environment (its python, its cc, ...) shadows the host's.
PATH="$_tc/cargo/bin:$_tc/zig:$_tc/bin:$_tc/qemu/usr/bin:$PATH"
export PATH

# Caches that would otherwise land in ~/.cache and ~/.gradle (cargo-xwin's ~1 GB
# MSVC download, zig's cache) stay inside the toolchain directory.
export XDG_CACHE_HOME="$_tc/cache"
export GRADLE_USER_HOME="$_tc/gradle"

# The first connection reset otherwise throws away a gigabyte of download.
export XWIN_HTTP_RETRIES="${XWIN_HTTP_RETRIES:-12}"

# Set only for what is installed, so the APK step is skipped (loudly, by
# release.sh) rather than failing half-way on a machine without Android.
[ -d "$_tc/conda/lib/jvm" ] && export JAVA_HOME="$_tc/conda/lib/jvm"
if [ -x "$_tc/android/cmdline-tools/latest/bin/sdkmanager" ]; then
  export ANDROID_HOME="$_tc/android"
  [ -d "$_tc/android/ndk/$CDASH_ANDROID_NDK_VERSION" ] &&
    export NDK_HOME="$_tc/android/ndk/$CDASH_ANDROID_NDK_VERSION"
fi

# rustc and zig are memory hungry together with gradle; 4 jobs keeps a 5 GB
# machine from swapping. Raise it with CARGO_BUILD_JOBS on a bigger one.
if [ -z "${CARGO_BUILD_JOBS:-}" ]; then
  _n=$(nproc 2>/dev/null || echo 4)
  export CARGO_BUILD_JOBS=$(( _n < 4 ? _n : 4 ))
  unset _n
fi

unset _tc
