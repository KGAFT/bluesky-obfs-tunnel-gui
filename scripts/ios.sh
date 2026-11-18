#!/usr/bin/env bash
# Invoked by Xcode after generating ios/BlueskyTunnel.xcodeproj with XcodeGen.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
if [[ "$(uname -s)" != Darwin ]]; then
    echo 'iOS builds need macOS and the Xcode iOS SDK.' >&2
    exit 1
fi
: "${TARGET_BUILD_DIR:?Run this script from the generated Xcode project}"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH:$HOME/.cargo/bin"
export CARGO_TARGET_DIR="$DERIVED_FILE_DIR/cargo"
export CARGO_PROFILE_RELEASE_DEBUG=1
export CARGO_PROFILE_RELEASE_STRIP=none
profile=debug
build_args=()
if [[ "$CONFIGURATION" != Debug ]]; then profile=release; build_args+=(--release); fi
executables=()
for arch in $ARCHS; do
    case "$arch:${PLATFORM_NAME:-}" in
        arm64:iphoneos) rust_target=aarch64-apple-ios ;;
        arm64:iphonesimulator) rust_target=aarch64-apple-ios-sim ;;
        x86_64:iphonesimulator) rust_target=x86_64-apple-ios ;;
        *) echo "Unsupported iOS build architecture: $arch / $PLATFORM_NAME" >&2; exit 1 ;;
    esac
    cargo build --locked --bin bluesky-obfs-t-gui --target "$rust_target" "${build_args[@]}"
    executables+=("$CARGO_TARGET_DIR/$rust_target/$profile/bluesky-obfs-t-gui")
done
mkdir -p "$TARGET_BUILD_DIR"
lipo -create "${executables[@]}" -output "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
if [[ -n "${DWARF_DSYM_FOLDER_PATH:-}" && -n "${DWARF_DSYM_FILE_NAME:-}" ]]; then
    mkdir -p "$DWARF_DSYM_FOLDER_PATH"
    dsymutil "$TARGET_BUILD_DIR/$EXECUTABLE_PATH" -o "$DWARF_DSYM_FOLDER_PATH/$DWARF_DSYM_FILE_NAME"
fi
# The executable is replaced by Cargo, so force its signature to match the bundle.
if [[ "${CODE_SIGNING_ALLOWED:-YES}" == YES && -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ]]; then
    signing_args=()
    entitlements_path="$TARGET_TEMP_DIR/$PRODUCT_NAME.app.xcent"
    if [[ -s "$entitlements_path" ]]; then signing_args+=(--entitlements "$entitlements_path"); fi
    codesign --force --sign "$EXPANDED_CODE_SIGN_IDENTITY" "${signing_args[@]}" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
fi
