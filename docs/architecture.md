# Architecture

`ui/app.slint` defines the shared desktop/mobile interface; `ui/components.slint` holds its palette, buttons, and fields. `src/app.rs` binds UI properties and actions to profiles and connection commands. UI access stays on Slint's thread. A timer drains events from the Tokio worker runtime.

`src/engine.rs` owns the network runtime and session lifecycle. A command actor serializes starts and stops, awaits cancellation before acknowledging a stop, and tags events by generation so a previous session can't update the next session's UI. It binds the local listener once, authenticates through `ForwardClient`, and supervises connection retries, driver failure, and scheduled rotation. On mobile, the runtime and in-memory profile cache live at process scope, independent of Activity/UI recreation. A snapshot restores the connection status when the UI reattaches. Listener tasks and the tunnel driver are aborted on disconnect or process exit.

`src/profiles.rs` owns validation, platform storage, atomic profile writes, password persistence, embedded pattern installation, and relative path resolution. A profile wraps the existing library's `ClientSettings` with a name, forwarding mode, destination, and password-storage preference. The JSON editor serializes only `ClientSettings`, preserving compatibility with the terminal client's configuration.

Form drafts autosave with a short debounce. Connect, profile navigation, the password preference, window closing, and native background events also flush changes; drafts do not require connection validation. Files and directory renames are synced, and unremembered passwords are stripped only from the disk copy. Mobile UI recreation preserves session-only credentials in memory. Pattern imports validate through the library before publishing a unique private copy and persisting its relative path.

`src/platform.rs` bridges Java via JNI on Android and Objective-C on iOS. The Android Gradle app packages the NativeActivity subclass, Storage Access Framework picker, and foreground service with notification disconnect and renewable wake lock. Engine stop/failure cleans up the service without depending on UI polling. UIKit provides coordinated document import, lifecycle flushing, and an expiring background task. iOS continuous background tunneling still requires a separate VPN Network Extension implementation.

## Tunnel code provenance

The Cargo dependency uses `../bluesky-obfs-tunnel` with `default-features = false`, excluding Diesel/MariaDB. Protocol encoding, authentication, obfuscation, configuration validation, multiplexing, and flow control remain in that library and its dependencies.

The CLI listener modules aren't exported by the upstream library. These source files and their existing regression tests were copied without behavioral changes from the sibling checkout at commit `a07ca36f8d0786f4e73d79a5962d0605082789ff`:

- `src/bin/client/http_proxy/` → `src/http_proxy/`
- `src/bin/client/tcp_forward.rs` and `tcp_forward/` → corresponding paths under `src/`
- `src/bin/client/udp_forward.rs` and `udp_forward/` → corresponding paths under `src/`

They preserve bounded HTTP headers, CONNECT read-ahead, streaming uploads, TCP half-closes, per-sender UDP associations, queue limits, and listener behavior during rotation. Keeping their tests in this crate verifies the actual adapters used by the GUI. When updating the sibling project, sync these modules and rerun the tests. A future upstream library API for the three listeners would remove this duplication.

The sibling checkout was left unchanged. Both captured patterns are embedded at build time and provisioned into app storage on first launch. A deployed executable therefore doesn't read paths from the build machine.

## Validation limits

Local tests exercise protocol traffic against in-memory framed sessions and local TCP/UDP sockets. They don't perform a production FakeCodec/MariaDB handshake. The engine's lifecycle tests also exercise cancellation of an in-progress setup against a nonresponding local socket. Test with a real multiplexed server using your matching cover settings, credentials, and captured patterns before relying on a deployment.
