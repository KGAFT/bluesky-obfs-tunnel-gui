# Platform builds

Checks made on Linux on 2026-11-18. Build checks do not replace device lifecycle testing.

| Platform | Result in this workspace | What remains |
| --- | --- | --- |
| Linux x86_64 | `cargo check`, desktop build, protocol/config/UI tests, software rendering at 1180×820, 390×844, and 360×640 | Live authentication with your server and credentials |
| Android ARM64 | Rust `cargo check`, native debug/release libraries, signed debug APK, crypto-optimized unsigned release APK, Gradle lint (no errors), 16 KiB ELF/APK alignment | Device tests for foreground service, file picker, notification disconnect, Doze and Activity recreation; on-device performance benchmarks |
| iOS ARM64 / simulator | Cross-check stops with missing `std` / `core` for `aarch64-apple-ios`; host is Linux | macOS, Xcode with iOS SDK/simulator, Rust iOS targets, XcodeGen; signing team/profile for a device |
| Windows / macOS desktop | Winit backend configured; not built on these hosts | Native toolchains and native builds; availability of a Rust cross-target alone doesn't verify the app |

## Android

The library exports `android_main`, initializes Slint's Android Activity 0.6 backend, and uses private app storage. `android/` packages a Java NativeActivity subclass for document import and a foreground service for persistent connections. This app requires Android 8/API 26 or newer. Slint's Android backend uses Skia; NDK r30 or newer avoids C++ runtime mismatches with its prebuilt renderer. See [Slint's Android guide](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/android/).

Install these through Android Studio's SDK Manager:

- Android SDK platform 35 and corresponding build tools (the manifest targets API 35).
- Android NDK r30 or newer.
- Platform tools and an ARM64 device or ARM64 emulator.

Also install Rust, JDK 17+, Gradle 8.11.1–8.14.x (AGP 8.9.1), CMake, Perl, Git, and libclang, then:

```sh
rustup target add aarch64-linux-android
export ANDROID_HOME="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/YOUR_INSTALLED_VERSION"
export JAVA_HOME=/path/to/jdk
bash scripts/android.sh check
bash scripts/android.sh build debug
bash scripts/android.sh run debug
# Optimized release for devices with ARMv8 crypto and CRC extensions:
BLUESKY_ARM64_PRESET=crypto bash scripts/android.sh build release
```

The script sets the NDK variables used by Slint, Skia, and BoringSSL, API 26 C/C++ compilers, bindgen arguments, and the BoringSSL CMake toolchain. It isolates the NDK's C++ static archives and requests 16 KiB ELF page alignment. It builds the Rust library and packages it with Java sources through Gradle. APK output is under `android/build/outputs/apk/`. Debug builds use debug signing; release APKs are unsigned and require your own signing key and `apksigner`. Set `GRADLE` to a Gradle launcher if it is not on PATH. Direct `cargo apk` builds omit the Java bridge and service and are unsupported.

ARM64 optimization presets (`BLUESKY_ARM64_PRESET`):

| Preset | Rust flags | Native C/C++ flags | CPU requirements |
| --- | --- | --- | --- |
| `portable` (default) | `+neon`, `--cfg aes_armv8`, `--cfg polyval_armv8` | `-O3 -march=armv8-a -moutline-atomics` | Standard Android ARM64; AES/PMULL selected at runtime with software fallback |
| `crypto` | Above plus `+aes,+sha2,+crc` | `-O3 -march=armv8-a+crypto+crc -moutline-atomics` | AES, PMULL, SHA1/SHA256 and CRC extensions; unsupported CPUs can fault |

The locked `aes 0.8` and `polyval 0.6` crates require the `aes_armv8` / `polyval_armv8` configuration flags to compile their accelerated ARM backends. Their `cpufeatures` dependency supports Android runtime detection. Rust's AArch64 `aes` feature covers both AES and PMULL; there is no separate Rust `+pmull` flag. See the [Rust target-feature reference](https://doc.rust-lang.org/reference/attributes/codegen.html#target_feature). The iOS ARM64 device/simulator Cargo configuration also enables these crypto backends, retaining the Apple target's CPU defaults; iOS builds remain unverified here.

Both presets retain generic CPU tuning, rather than using the build host's CPU. Outlined atomics in native C/C++ can select LSE instructions on supported devices without requiring LSE globally. Rust's unstable `outline-atomics` target-feature flag is deliberately omitted. Releases retain optimization level 3, full LTO, one codegen unit and stripped symbols, and enable LLD `-O2`, unused-section removal and safe identical-code folding. `CARGO_PROFILE_RELEASE_LTO=thin` can trade some link optimization for faster builds.

There is no compiler switch that makes socket syscalls faster. Existing HTTP/TCP sockets already use `TCP_NODELAY`, bounded queues and bounded tunnel chunks. SIMD and optimized atomics help CPU/memory work around async I/O; CPU flags are not a measured network-throughput claim. Benchmark CPU time, sustained throughput and battery use on the intended device before judging the benefit. Protocol batching, socket buffer changes or `io_uring` would require separate runtime changes and validation.

Only ARM64 is configured. An x86_64 emulator needs its Rust target, corresponding compiler/toolchain settings, and Gradle ABI filter added.

On a phone, use the server's reachable address; `127.0.0.1` refers to the phone. Test the keyboard, scrolling, orientation changes, connection/retry/disconnect, and actual HTTP/TCP/UDP traffic. Import both patterns through Android's Storage Access Framework, then remove the originals and reconnect to verify the private copies.

The connection starts a `specialUse` foreground service (API 34+ type/permission declared) while the activity is visible. It owns an ongoing notification with a Disconnect action and a timed, renewable partial wake lock. The Rust runtime lives at process scope; the service is stopped from the runtime on disconnect or terminal failure, without waiting for the UI timer. The service uses `START_NOT_STICKY`, so it cannot restart an empty session after process death. Android backup is disabled to keep remembered credentials out of OS backups. Publishing to Google Play requires review of the declared special-use foreground service purpose; see [Android service types](https://developer.android.com/about/versions/14/changes/fgs-types-required#special-use).

In app Settings, open **Battery optimization settings**, then Android **Battery → Unrestricted**. A foreground service/wake lock does not exempt an app from every Doze or vendor restriction. Test with the screen off, forced Doze, network changes, Activity recreation and notification disconnect. Force stop still terminates the session. This app is a local proxy/forwarder, not a `VpnService`.

## iOS

The normal Rust executable is used with Slint's Winit and Skia backends. The XcodeGen template is in `ios/project.yml`, with a Cargo build phase in `scripts/ios.sh`. It includes safe-area support through the shared UI, an iPhone/iPad target, local-network usage text, and a Slint scene delegate declaration. Packaging follows [Slint's iOS guide](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/).

On a Mac:

```sh
xcode-select --install
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
brew install xcodegen cmake llvm
cargo build --locked --bin bluesky-obfs-t-gui --target aarch64-apple-ios-sim
cd ios
xcodegen generate
open BlueskyTunnel.xcodeproj
```

Install full Xcode and its iOS platform components; command-line tools alone don't supply the iOS SDK. Select an iPhone simulator in Xcode and build. Intel Mac simulators additionally need `x86_64-apple-ios`. For a physical device, choose your bundle identifier and signing team, install the device target, and select the device in Xcode. App Store/TestFlight distribution requires the appropriate Apple account and provisioning.

The build script selects device/simulator targets, builds the Rust binary, places it in the bundle, generates debug symbols, and signs the executable when Xcode provides an identity. Its syntax is checked here, but Xcode generation, linking, scene lifecycle, signing, and launch need validation on macOS. The locked Slint/Winit version must be checked against the iOS version you intend to ship.

`ios/Platform.m` is compiled into the Rust executable by `build.rs`; it supplies a UIKit Files picker, coordinated/security-scoped reading, profile flushing on resignation, and `beginBackgroundTask` for the OS-granted grace period. On expiration, the tunnel is stopped and the UI reports the expiration on return. Re-entering the foreground before expiration ends the background task and retains the session. No fake audio/location background modes are declared.

This does **not** provide indefinite background tunneling. A VPN Network Extension would require an additional signed extension target, entitlements, shared configuration and a packet-routing implementation for this stream-based protocol. See [Apple's background execution guidance](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time) and [Network Extension setup](https://developer.apple.com/documentation/xcode/configuring-network-extensions/). Verify Files import, remembered credentials after termination, background expiration, signing, and local proxy access on a real iPhone.

## Desktop and UI checks

```sh
cargo build --locked --bin bluesky-obfs-t-gui
cargo test --locked
# Generate deterministic layout previews without opening a desktop window:
cargo test --locked --test ui_render
```

Desktop navigation changes to bottom tabs below 760 logical pixels. Phone pages scroll, forms sit inside a scroll view for virtual-keyboard visibility, and window safe-area insets keep controls away from system overlays. Host-rendered screenshots verify layout; they don't validate a mobile backend or touch/keyboard behavior on a device.
