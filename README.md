# Bluesky Tunnel

A Rust + Slint client for `../bluesky-obfs-tunnel`. A dark, touch-friendly interface adapts from a desktop sidebar to phone navigation below 760 logical pixels. It runs the tunnel in-process, without launching the terminal client.

- HTTP/HTTPS proxy, TCP forwarding, and UDP forwarding.
- Multiple local profiles, masked passwords, and opt-in credential storage.
- Live authentication, reconnect, failure, and session uptime indicators.
- Automatic session rotation and persistent local listeners during reconnects.
- An editable JSON configuration compatible with the terminal client's `client.json`.

Rendered previews: [desktop](docs/screenshots/desktop.png), [phone](docs/screenshots/mobile.png), and [phone profile editor](docs/screenshots/mobile-profile.png).

## Run on desktop

Keep both repositories next to one another. The sibling library and its shipped pattern pair are required at build time. The built executable embeds the pair and doesn't need the sibling checkout at runtime.

```sh
cargo run --locked
# Preview the phone layout in a desktop window:
cargo run --locked -- --mobile
```

Use a recent Rust toolchain (tested with 1.101.0-nightly). Building the tunnel's BoringSSL dependency also needs a C/C++ compiler, CMake, Perl, Git, and libclang. Linux needs the normal Wayland/X11 development environment and fontconfig. The desktop uses Slint's Winit backend and software renderer; no Qt installation is needed.

Open **Profiles**, enter your server address and server-issued identity/password, choose a mode and local listener, and save. Then connect from **Overview**. Your server must use multiplexed mode. For HTTP mode, configure the consuming application's HTTP and HTTPS proxy to the local listener, such as `127.0.0.1:8080`.

For TCP/UDP modes, the destination is reached from the **server's** network. A destination of `127.0.0.1:8090` means port 8090 on the server. Only applications using the local proxy or forwarding port send traffic through this client.

To reuse an existing terminal configuration, paste its contents in **Settings → Apply JSON**. Use **Load from form** to regenerate JSON after editing the form. Applying JSON saves the configuration, respecting **Remember password**. Omitted or unchanged identity/password fields keep the current form's credentials, so an older editor snapshot cannot roll them back. Explicitly edited JSON credentials replace the form values. Advanced fields such as public password, server ID, cover site, queues, pattern paths, and retry intervals are preserved when editing the regular form.

## Storage

Linux uses `$XDG_CONFIG_HOME/bluesky-tunnel` or `~/.config/bluesky-tunnel`; macOS/iOS use `~/Library/Application Support/bluesky-tunnel`; Windows uses `%APPDATA%/bluesky-tunnel`. Android uses the app's private internal directory. `BLUESKY_GUI_DATA_DIR` overrides the directory for desktop development.

Form changes save automatically after a short pause, including incomplete drafts. Connecting, switching profiles, changing **Remember password**, closing, and mobile background transitions also flush changes. **Save profile** saves immediately. Connection validation happens when connecting, so unfinished settings cannot prevent credential storage.

Passwords stay in memory unless **Remember password** is enabled. Remembered credentials are plaintext in `profiles.json`, written atomically with mode 0600 on Unix; both file contents and the rename are synced before success. Turning remembering off removes the saved password immediately while retaining it for the current process. Without remembering, enter the password again after restarting the app. Activity is kept in memory, with the last 100 connection events; stream diagnostics go to stderr.

The default captured patterns are installed on first launch without overwriting existing files. To use another pair, open **Settings → Pattern files**. On desktop, enter a readable source path and press **Import client pattern** / **Import server pattern**. On Android/iOS, these buttons open the system document picker, including shared/cloud files. Each file is checked with the tunnel library and copied into private storage under a unique relative path for the current profile. Deleting the original file or importing for another profile does not affect that copy. Empty, invalid, and files larger than 16 MiB are rejected. Import both matching files; the cover settings, server ID, public password, browser emulation, and pattern pair must match the server.

Android connections use a foreground service with an ongoing notification, a **Disconnect** action, and a CPU wake lock while connecting/connected/retrying. **Settings → Battery optimization settings** opens the app's Android settings; set battery usage to **Unrestricted**. Disconnect/failure releases the service and lock. Activity teardown does not own the tunnel runtime. Force stop or process death still ends the session; the app does not silently restart using credentials that you chose not to store.

iOS uses UIKit's limited background grace period and ends the tunnel when it expires, with a notice on return. Continuous background networking requires a separately implemented VPN Network Extension and appropriate signing/entitlements; this local proxy build does not provide it. It does not claim that a timer or background-mode flag can bypass iOS suspension.

## Build status

See [platform builds and requirements](docs/platforms.md) for checks, Android service packaging, and the iOS Xcode project. Android Rust and Java compilation are checked on this host. Mobile lifecycle behavior still needs device testing; iOS compilation requires macOS/Xcode.

Android release builds use `bash scripts/android.sh build release`. The default portable ARM64 preset enables runtime-selected AES/PMULL acceleration; `BLUESKY_ARM64_PRESET=crypto` additionally requires hardware AES/PMULL, SHA and CRC instructions. Both use NEON and native C/C++ outlined atomics. See the platform guide for CPU compatibility and unsigned release APK output.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Tests cover the imported HTTP/TCP/UDP adapters, configuration persistence, cancellation, and UI behavior. The rendering test writes desktop and phone PNGs to `target/previews/` without requiring a display server. These local protocol tests don't authenticate against a live production server.

See [architecture and source provenance](docs/architecture.md).
