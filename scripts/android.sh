#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
usage() {
    cat <<'EOF'
Usage: bash scripts/android.sh [check|native|build|run] [debug|release]

  check          Check Android code (debug by default).
  native         Produce the native .so without packaging or signing (release by default).
  build          Produce an APK (release by default).
  run            Build, install, and run an APK (release by default).

Use debug/--debug or release/--release to select the build profile.
Release uses Cargo.toml's release settings (opt-level 3, full LTO, one codegen unit,
stripped symbols). Set CARGO_PROFILE_RELEASE_LTO=thin for faster linking.
BLUESKY_ARM64_PRESET=portable (default): NEON, native outlined atomics and runtime-selected
ARMv8 AES/PMULL backends. No optional CPU instructions are required globally.
BLUESKY_ARM64_PRESET=crypto: also require AES/PMULL, SHA1/SHA256 and CRC instructions
in Rust and C/C++ dependencies. Use only on devices that support those extensions.
Native output: target/aarch64-linux-android/<profile>/libbluesky_obfs_t_gui.so
              (or under CARGO_TARGET_DIR when set).
APK builds use the Java Activity, file picker and foreground service in android/.
Install Gradle 8.11.1 and JDK 17+ for packaging (or set GRADLE to its launcher).
Release APKs are unsigned; sign with your own key before installation.
Examples:
  bash scripts/android.sh native release
  bash scripts/android.sh build debug
  bash scripts/android.sh build release
  BLUESKY_ARM64_PRESET=crypto bash scripts/android.sh build release
  bash scripts/android.sh run debug
EOF
}

action="${1:-check}"
case "$action" in
    check|native|build|run) ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac
if (( $# > 2 )); then
    usage >&2
    exit 2
fi
profile="${2:-release}"
if [[ "$action" == check && $# -lt 2 ]]; then
    profile=debug
fi
profile_args=()
case "$profile" in
    debug|--debug) profile=debug ;;
    release|--release) profile=release; profile_args=(--release) ;;
    *) usage >&2; exit 2 ;;
esac

arm64_preset="${BLUESKY_ARM64_PRESET:-portable}"
case "$arm64_preset" in
    portable)
        arm64_features=+neon
        native_arch=armv8-a
        ;;
    crypto)
        arm64_features=+neon,+aes,+sha2,+crc
        native_arch=armv8-a+crypto+crc
        ;;
    *) echo 'BLUESKY_ARM64_PRESET must be portable or crypto.' >&2; exit 2 ;;
esac

: "${ANDROID_HOME:?Set ANDROID_HOME to the Android SDK directory}"
: "${ANDROID_NDK_HOME:?Install NDK r30 or newer and set ANDROID_NDK_HOME}"
export ANDROID_HOME ANDROID_NDK_HOME
# cargo-apk prefers the deprecated variable over ANDROID_HOME; use one SDK.
unset ANDROID_SDK_ROOT
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
export ANDROID_NDK="$ANDROID_NDK_HOME"

if [[ "$action" == build || "$action" == run ]]; then
    target_sdk=$(awk '
        /^\[package\.metadata\.android\.sdk\][[:space:]]*(#.*)?$/ { sdk = 1; next }
        /^\[/ { sdk = 0 }
        sdk && /^[[:space:]]*target_sdk_version[[:space:]]*=/ {
            sub(/^[^=]*=[[:space:]]*/, "")
            sub(/[[:space:]#].*$/, "")
            print
            exit
        }
    ' Cargo.toml)
    if [[ ! "$target_sdk" =~ ^[0-9]+$ ]]; then
        echo 'Set an integer target_sdk_version in [package.metadata.android.sdk] in Cargo.toml.' >&2
        exit 1
    fi
    platform_jar="$ANDROID_HOME/platforms/android-$target_sdk/android.jar"
    if [[ ! -f "$platform_jar" ]]; then
        echo "Missing SDK platform required by Cargo.toml: $platform_jar" >&2
        echo "Install Android SDK Platform API $target_sdk using Android Studio's SDK Manager." >&2
        echo "With Android SDK Command-line Tools installed, run:" >&2
        printf '  sdkmanager --sdk_root=%q %q\n' "$ANDROID_HOME" "platforms;${target_sdk}" >&2
        echo 'cargo-apk ignores platform directories with non-integer suffixes such as android-37.1.' >&2
        exit 1
    fi
    # Use the same platform for Slint Java compilation and APK packaging.
    export ANDROID_JAR="$platform_jar"
fi

case "$(uname -s)" in
    Linux) ndk_host=linux-x86_64 ;;
    Darwin) ndk_host=darwin-x86_64 ;;
    *) echo 'Use Linux or macOS with the matching NDK package.' >&2; exit 1 ;;
esac
ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$ndk_host/bin"
if [[ ! -x "$ndk_bin/aarch64-linux-android26-clang" ]]; then
    echo "Missing Android API 26 compiler under $ndk_bin" >&2
    exit 1
fi
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android26-clang"
export CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android26-clang"
export CXX_aarch64_linux_android="$ndk_bin/aarch64-linux-android26-clang++"
export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
export RANLIB_aarch64_linux_android="$ndk_bin/llvm-ranlib"
export CMAKE_TOOLCHAIN_FILE_aarch64_linux_android="$project_dir/scripts/android-aarch64.cmake"
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android=--target=aarch64-linux-android26
export BORING_BSSL_RUST_CPPLIB_aarch64_linux_android=static=c++_static
# Outlined atomics select LSE at runtime, retaining ARMv8.0 compatibility.
export CFLAGS_aarch64_linux_android="${CFLAGS_aarch64_linux_android:-} -O3 -march=$native_arch -moutline-atomics"
export CXXFLAGS_aarch64_linux_android="${CXXFLAGS_aarch64_linux_android:-} -O3 -march=$native_arch -moutline-atomics"

# Only expose the C++ archives: adding the entire NDK sysroot library directory
# would accidentally select its static libc instead of Android's shared libc.
runtime_dir="${CARGO_TARGET_DIR:-target}/android-cxx-runtime"
mkdir -p "$runtime_dir"
runtime_dir="$(cd "$runtime_dir" && pwd)"
ndk_lib="$ndk_bin/../sysroot/usr/lib/aarch64-linux-android"
cp "$ndk_lib/libc++_static.a" "$ndk_lib/libc++abi.a" "$runtime_dir/"
rust_flags=(
    -C target-cpu=generic
    -C "target-feature=$arm64_features"
    --cfg aes_armv8 --cfg polyval_armv8
    -L "native=$runtime_dir"
    -C link-arg=-lc++abi
    -C link-arg=-Wl,-z,max-page-size=16384
)
if [[ "$profile" == release ]]; then
    # LLD removes unused/identical code without changing I/O semantics.
    rust_flags+=(-C link-arg=-Wl,-O2,--gc-sections,--icf=safe)
fi
printf -v CARGO_ENCODED_RUSTFLAGS '%s\x1f' "${rust_flags[@]}"
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}"
printf 'Android ARM64 preset: %s (%s); profile: %s\n' "$arm64_preset" "$arm64_features" "$profile"

if [[ "$action" == check ]]; then
    cargo check --locked --lib --target aarch64-linux-android "${profile_args[@]}"
elif [[ "$action" == native ]]; then
    cargo build --locked --lib --target aarch64-linux-android "${profile_args[@]}"
    printf 'Native library: %s/aarch64-linux-android/%s/libbluesky_obfs_t_gui.so\n' "${CARGO_TARGET_DIR:-target}" "$profile"
else
    if ! command -v "${GRADLE:-gradle}" >/dev/null 2>&1; then
        echo 'Install Gradle 8.11.1, or set GRADLE to its launcher.' >&2
        exit 1
    fi
    cargo build --locked --lib --target aarch64-linux-android "${profile_args[@]}"
    mkdir -p android/native-libs/arm64-v8a
    cp "${CARGO_TARGET_DIR:-target}/aarch64-linux-android/$profile/libbluesky_obfs_t_gui.so" android/native-libs/arm64-v8a/
    if [[ "$profile" == debug ]]; then gradle_task=assembleDebug; else gradle_task=assembleRelease; fi
    "${GRADLE:-gradle}" --project-dir android "$gradle_task"
    if [[ "$action" == run ]]; then
        if [[ "$profile" != debug ]]; then
            echo 'Release APK is unsigned. Sign it with apksigner before installing, or use run debug.' >&2
            exit 1
        fi
        "$ANDROID_HOME/platform-tools/adb" install -r android/build/outputs/apk/debug/BlueskyTunnel-debug.apk
        "$ANDROID_HOME/platform-tools/adb" shell am start -n app.bluesky.tunnel/.TunnelActivity
    fi
fi
