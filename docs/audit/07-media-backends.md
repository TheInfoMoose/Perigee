# 07 Media Backends and Display Modes audit
Audited HEAD: 0a69a00

## Summary
- Every platform display capturer (`WaylandDmabufCapturer`, `X11ShmCapturer`, `WindowsDxgiCapturer`, `MacOsScreenCaptureKitCapturer`) is a stub that drops its OS connection/COM objects during construction and returns a constant-filled `Vec<u8>` (`0x80`, `0x40`, `0x7F`, `0x90`) capped at `64 KiB` on every `capture_next_frame()` call (`crates/rferram-server/src/backends/linux_capture.rs:277-302, 403-428`, `windows.rs:564-590`, `macos.rs:360-386`).
- Both video encoders (`SoftwareVideoEncoder` and `MacOsVideoToolboxEncoder`) slice the first `1024` or `2048` bytes of `frame.data` without compression (`crates/rferram-server/src/encoder.rs:83-84, 153-154`); `rav1e` is declared in root `Cargo.toml:94` and required by `.agents/skills/rferram-capture-and-compositor/SKILL.md:30` but is not a dependency in `crates/rferram-server/Cargo.toml`.
- `HostAudioCapturer` (`crates/rferram-server/src/audio.rs:51-94`) sleeps 20 ms per tick and emits `vec![0u8; 192]` tagged as `AudioCodec::Opus` (`48 kHz` stereo) with no OS audio loopback API (`PipeWire`, `WASAPI`, `CoreAudio`) or Opus encoder in the workspace.
- Linux input injection (`WaylandVirtualInputInjector` and `X11XTestInputInjector` in `crates/rferram-server/src/backends/linux_input.rs:557-700, 864-982`) only updates in-memory atomics/mutexes and logs `debug!`; Windows `SendInput` (`windows.rs:1048`) and macOS `CGEventPost` (`macos.rs:728-794`) make real FFI calls when compiled on their target OS, but hardcode `2560x1440` geometry (`crates/rferram-server/src/lib.rs:107-118`), disagree with Linux on normalized `[0.0, 1.0]` vs pixel coordinates, and map only 19 (`windows.rs:661-682`) and 12 (`macos.rs:470-484`) hardware scancodes.
- Neither headless virtual display creation nor session takeover is implemented on any OS, and `FrameCapturer::enumerate_monitors()` / `WireMessage::Topology` are never called or sent outside unit tests.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Capture — Linux Wayland (`WaylandDmabufCapturer`) | Stub | `crates/rferram-server/src/backends/linux_capture.rs:198-303`; `probe_wayland_environment()` (`:234-251`) checks `Connection::connect_to_env().is_ok()` and drops the connection without binding `zwp_linux_dmabuf_v1` or `zwlr_screencopy_manager_v1`; `capture_next_frame()` (`:277-302`) returns `vec![0x80; payload_len]` capped at `64 KiB` (`64 * 1024`) | R2, R6, R12 |
| Capture — Linux X11 (`X11ShmCapturer`) | Stub | `crates/rferram-server/src/backends/linux_capture.rs:319-429`; `probe_x11_display()` (`:356-377`) checks `x11rb::connect(None)` and `shm_query_version()` then drops `conn`; `capture_next_frame()` (`:403-428`) never calls `shm::get_image` or `xproto::get_image`, returning `vec![0x40; payload_len]` capped at `64 KiB` | R2, R6, R12 |
| Capture — Windows (`WindowsDxgiCapturer`) | Stub | `crates/rferram-server/src/backends/windows.rs:352-591`; `probe_windows_dxgi_output()` (`:406-538`) creates `ID3D11Device` and `IDXGIOutputDuplication` to read `GetDesc()` (`:470-472`) and immediately drops all COM objects (`:480`); `capture_next_frame()` (`:564-590`) never calls `AcquireNextFrame`, returning `vec![0x7F; payload_len]` capped at `64 KiB` | R2, R6, R12 |
| Capture — macOS (`MacOsScreenCaptureKitCapturer`) | Stub | `crates/rferram-server/src/backends/macos.rs:226-387`; `probe_macos_screencapturekit()` (`:272-334`) checks `AnyClass::get(c"SCShareableContent")` / `AnyClass::get(c"SCStream")` and `CGMainDisplayID()` dimensions without instantiating an `SCStream`; `capture_next_frame()` (`:360-386`) returns `vec![0x90; payload_len]` capped at `64 KiB` | R2, R6, R12 |
| Video encode (`SoftwareVideoEncoder` / `MacOsVideoToolboxEncoder`) | Stub | `crates/rferram-server/src/encoder.rs:75-117, 145-179`; `MacOsVideoToolboxEncoder::encode` copies `frame.data[..min(2048, len)]` (`:83-84`) and `SoftwareVideoEncoder::encode` copies `frame.data[..min(1024, len)]` (`:153-154`); `rav1e` is absent from `crates/rferram-server/Cargo.toml` | R2, R6, R12 |
| Audio capture — Linux (`PipeWire`) | Stub | `crates/rferram-server/src/audio.rs:18-99`; `HostAudioCapturer` stores `backend: CaptureBackendType` (`:32`) only for `info!(?backend, ...)` (`:74`) and emits `vec![0u8; 192]` every `20 ms` (`:59, 77-85`) with no PipeWire/PulseAudio/ALSA or Opus binding | R6, R12 |
| Audio capture — Windows (`WASAPI`) | Stub | `crates/rferram-server/src/audio.rs:18-99` and `crates/rferram-server/Cargo.toml:36-38`; workspace `Cargo.toml:79` enables `Win32_Media_Audio`, but `audio.rs` calls no WASAPI (`IMMDeviceEnumerator`/`IAudioCaptureClient`) or Opus APIs and emits `vec![0u8; 192]` every `20 ms` | R6, R12 |
| Audio capture — macOS (`CoreAudio` / `ScreenCaptureKit`) | Stub | `crates/rferram-server/src/audio.rs:18-99`; emits `vec![0u8; 192]` every `20 ms` with no `CoreAudio`/`AudioUnit`/`SCStreamOutputType::Audio` or Opus integration | R6, R12 |
| Input injection — Linux Wayland (`WaylandVirtualInputInjector`) | Stub | `crates/rferram-server/src/backends/linux_input.rs:438-700`; `probe_wayland_virtual_input()` (`:502-529`) drops its `wayland_client::Connection` and `xkb::Keymap` immediately; `inject_input()` (`:557-700`) only updates `last_pointer_coords`/`last_evdev_scancode`/`last_modifiers_mask`/`injected_count` and logs `debug!` | R2, R6, R12 |
| Input injection — Linux X11 (`X11XTestInputInjector`) | Stub | `crates/rferram-server/src/backends/linux_input.rs:716-982`; `probe_x11_xtest()` (`:776-797`) checks `xtest_get_version(2, 2)` and drops `conn`; `inject_input()` (`:864-982`) computes opcodes/coordinates and logs `debug!` without ever calling `xtest_fake_input` | R2, R6, R12 |
| Input injection — Windows (`WindowsSendInputInjector`) | Partial | `crates/rferram-server/src/backends/windows.rs:730-1089`; `dispatch_win32_send_input()` (`:989-1061`) calls Win32 `SendInput(&raw_inputs, cb_size)` (`:1048`) on Windows, but hardcodes `2560x1440` (`lib.rs:118`), expects pixel coordinates rather than normalized `[0.0, 1.0]` (`:628-641, 740`), maps only 19 scancodes (`:661-682`), and never switches to the Winlogon input desktop (`OpenInputDesktop`/`SetThreadDesktop`) | R2, R6, R12 |
| Input injection — macOS (`MacOsCgEventInjector`) | Partial | `crates/rferram-server/src/backends/macos.rs:488-828`; `post_cg_event_macos()` (`:662-802`) calls `CGEventCreate*` and `CGEventPost(K_CG_HID_EVENT_TAP, ev)` (`:728, 756, 774, 794`) on macOS, but hardcodes `2560x1440` (`lib.rs:113`), treats relative `MouseMove` deltas as absolute Quartz coordinates (`:500-510`), maps only 12 scancodes (`:470-484`), emits no key-up for `UnicodeText` (`:787-797`), and never checks TCC Accessibility permission | R2, R6, R12 |
| Display geometry & multi-monitor topology | Stub | `crates/rferram-server/src/lib.rs:107-118`, `crates/rferram-server/src/daemon.rs:79-92, 166-168, 595-598`, `crates/rferram-server/src/encoder.rs:26-27`; `2560x1440` is hardcoded across capture, encoder, and input injectors; `FrameCapturer::enumerate_monitors()` is never called outside `#[cfg(test)]`; `WireMessage::Topology` (`TopologyMessage::DisplayLayout` / `ResizeRequest`) is never sent or handled | R6, R12 |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-CAP-1: all capture backends return constant-filled buffers (`linux_capture.rs:299, :425; windows.rs:587; macos.rs:383`). | Confirmed | `crates/rferram-server/src/backends/linux_capture.rs:299` (`data: vec![0x80; payload_len]`), `linux_capture.rs:425` (`data: vec![0x40; payload_len]`), `windows.rs:587` (`data: vec![0x7F; payload_len]`), and `macos.rs:383` (`data: vec![0x90; payload_len]`), all capped at `64 * 1024` bytes (`64 KiB`), plus `daemon.rs:170` (`data: vec![128u8; 64 * 1024]`) on error | CAP-01, CAP-02 |
| L-ENC-1: the encoder copies the first 1024 / 2048 bytes (`encoder.rs:153, :83`); `rav1e` is not a server dependency. | Confirmed | `crates/rferram-server/src/encoder.rs:83-84` (`MacOsVideoToolboxEncoder` copies `frame.data[..min(2048, frame.data.len())]`), `encoder.rs:153-154` (`SoftwareVideoEncoder` copies `frame.data[..min(1024, frame.data.len())]`), and `crates/rferram-server/Cargo.toml:15-26` (omits `rav1e` despite root `Cargo.toml:94`) | ENC-01 |
| L-AUD-1: audio frames are zero-filled 192-byte buffers every 20 ms (`audio.rs:28, :59`). | Confirmed | `crates/rferram-server/src/audio.rs:19, 28, 59, 71-90` (`OPUS_FRAME_INTERVAL_MS = 20`, `OPUS_PAYLOAD_BYTES = 192`, `build_audio_frame` sets `payload: vec![0u8; OPUS_PAYLOAD_BYTES]`, and `start_loopback_capture` sends one every `20 ms` sleep tick) | AUD-01 |
| L-IN-1: Linux Wayland and X11 input injection only logs (`linux_input.rs:557+, :864+`); Windows `SendInput` (`windows.rs:1048`) and macOS `CGEventPost` (`macos.rs:728-794`) are real. | Confirmed | `crates/rferram-server/src/backends/linux_input.rs:557-700, 864-982` (only updates internal state and calls `debug!`); `crates/rferram-server/src/backends/windows.rs:989-1061` calls `unsafe { SendInput(&raw_inputs, cb_size) }` at `:1048`; `crates/rferram-server/src/backends/macos.rs:662-802` calls `CGEventPost(K_CG_HID_EVENT_TAP, ev)` at `:728, 756, 774, 794` (though both have coordinate/keymap/privilege defects) | IN-01, IN-02 |
| L-IN-2: remote screen size is hardcoded to 2560x1440 (`lib.rs:107-118`). | Confirmed | `crates/rferram-server/src/lib.rs:107-118` (`NativeInputInjector::new` passes literal `2560, 1440` to `LinuxInputController::new`, `MacOsCgEventInjector::new_live`, and `WindowsSendInputInjector::new_live`), reinforced by `encoder.rs:26-27` and `daemon.rs:79-92, 166-168, 595-598` | CAP-03 |
| L-SAFE-1: every `unsafe` block has a `// SAFETY:` comment (10 sites claimed). | Confirmed | `grep -rn "unsafe" crates/rferram-server/` returns exactly 10 lines: 7 `unsafe { ... }` blocks (`macos.rs:301, 720, 746, 769, 786`, `windows.rs:445, 1048`), every one of which has a preceding `// SAFETY:` comment (`macos.rs:297-300, 716-719, 743-745, 766-768, 783-785`, `windows.rs:440-444, 1044-1047`), plus 3 `unsafe extern "C"` FFI block declarations (`macos.rs:288, 674, 704`) whose call sites are covered by those 7 `// SAFETY:` comments (an 11th `unsafe` in the workspace at `crates/rferram-ui/src/lib.rs:69` also has `// SAFETY:` at `:66-68`) | — |

## Display modes

Per MUTATION_SPEC item 3, the OS mechanism column is marked **Suspected** as architectural input for the Phase 1 evaluation (R6, R12).

| Display Mode | OS / Display Server | What exists in code today (`Read`) | OS mechanisms required (`Suspected`) |
| :--- | :--- | :--- | :--- |
| **Screen share** (mirror physical session) | Linux Wayland | Stub only (`linux_capture.rs:185-303`, `linux_input.rs:422-700`): checks `wayland_client::Connection::connect_to_env().is_ok()` and drops the connection; never binds `zwlr_screencopy_manager_v1`, `zwp_linux_dmabuf_v1`, or PipeWire. | **Suspected:** Cross-desktop Wayland screen share requires `xdg-desktop-portal` (`org.freedesktop.portal.ScreenCast` + `org.freedesktop.portal.RemoteDesktop` over D-Bus/`zbus`) negotiating a `PipeWire` stream with DMA-BUF (`SPA_DATA_DmaBuf`) or MemFd buffers and `libei`/portal input; wlroots/COSMIC compositor-specific direct paths use `ext-image-copy-capture-v1` / `cosmic-screencopy-v2` / `zwlr_screencopy_manager_v1` plus `zwlr_virtual_pointer_v1` and `zwp_virtual_keyboard_v1` (or `/dev/uinput`). |
| **Screen share** (mirror physical session) | Linux X11 | Stub only (`linux_capture.rs:306-429`, `linux_input.rs:703-982`): probes `x11rb::connect(None)`, `shm_query_version()`, and `xtest_get_version(2, 2)`, then drops `conn`; never attaches SHM or calls `xtest_fake_input`. | **Suspected:** Persistent `x11rb` connection holding an `ipc::shmget`/`shmat` segment with `x11rb::protocol::shm::attach` + `shm::get_image` (and `xfixes` cursor/damage tracking + `randr::get_screen_resources_current` for monitor topology), paired with `x11rb::protocol::xtest::fake_input` + `conn.flush()`. |
| **Screen share** (mirror physical session) | Windows | Probe + input FFI only (`windows.rs:335-591, 954-1089`): `probe_windows_dxgi_output()` creates and immediately drops `ID3D11Device` and `IDXGIOutputDuplication` (`:445-480`); `SendInput` (`:1048`) runs on Windows in the caller's desktop. | **Suspected:** Retain `ID3D11Device`, `ID3D11DeviceContext`, and `IDXGIOutputDuplication` across frames; call `AcquireNextFrame`, copy or share the `ID3D11Texture2D` (into a hardware encoder or staging texture via `CopyResource` + `Map`/`ReleaseFrame`), handle `DXGI_ERROR_ACCESS_LOST` on mode/desktop switch, and run an elevated service/agent helper that calls `OpenInputDesktop` + `SetThreadDesktop` so `SendInput` works across UAC / Winlogon desktops. |
| **Screen share** (mirror physical session) | macOS | Probe + input FFI only (`macos.rs:211-387, 627-828`): checks `AnyClass::get` for `SCShareableContent`/`SCStream` and queries `CGMainDisplayID()` dimensions (`:294-312`); calls `CGEventPost(kCGHIDEventTap, ...)` (`:728-794`). | **Suspected:** Check/request TCC Screen Recording (`CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`) and Accessibility (`AXIsProcessTrustedWithOptions`); instantiate `SCShareableContent`, `SCContentFilter`, `SCStreamConfiguration`, and `SCStream` with an `SCStreamOutput` Objective-C delegate delivering `CMSampleBufferRef` -> `CVImageBufferRef` -> `IOSurfaceRef` directly to `VTCompressionSessionRef` (`VideoToolbox`). |
| **Headless** (virtual display) | Linux (Wayland / X11) | Missing: `"headless"` in `linux_capture.rs`/`linux_input.rs` refers only to returning synthetic constant arrays in CI when no display server is running. | **Suspected:** Standalone Smithay/`cosmic-comp` headless DRM-render-node (`DRM_NODE` / GBM / Vulkan) compositor session or virtual output (`wlr-output-management-unstable-v1` / PipeWire virtual monitor), or `Xvfb`/`Xorg` with `xf86-video-dummy` on X11, exporting DMA-BUF frames and receiving direct compositor seat input. |
| **Headless** (virtual display) | Windows | Missing: no virtual display driver or session creation exists in `windows.rs`. | **Suspected:** Indirect Display Driver (`IddCx` UMDF v2 driver) creating synthetic GPU-backed monitor outputs on the host adapter so `IDXGIOutputDuplication` can capture when no physical monitor is attached. |
| **Headless** (virtual display) | macOS | Missing: no virtual display creation exists in `macos.rs`. | **Suspected:** `CGVirtualDisplay` (`CGVirtualDisplayDescriptor` / `CGVirtualDisplaySettings` / `CGVirtualDisplayMode` in `CoreGraphics`) creating a synthetic Retina display targetable by `ScreenCaptureKit`. |
| **Session takeover** (lock physical console / attach to login or user session) | Linux / Windows / macOS | Missing: no PAM/`systemd-logind`, Windows RDS/Console session attach, or macOS screen-blanking/loginwindow agent exists anywhere in the repo. | **Suspected:** Linux: `systemd-logind` (`org.freedesktop.login1`) session control + compositor output blanking/DPMS or greeter handoff; Windows: `SYSTEM` service using `WTSQueryUserToken` / `CreateProcessAsUser` per active console/RDP session ID (`WTSGetActiveConsoleSessionId`) plus `SetThreadDesktop` for `Winlogon` lock screen; macOS: both a `LaunchDaemon` (`/Library/LaunchDaemons`) and a per-session `LaunchAgent` (`/Library/LaunchAgents` with `LimitLoadToSessionType` `Aqua` + `LoginWindow`) plus virtual display or display dimming. |

## Findings

### CAP-01: All four platform capture backends return constant-filled 64 KiB byte arrays instead of capturing display pixels
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-server/src/backends/linux_capture.rs:280-300`
```rust
        let payload_len = (stride as usize)
            .saturating_mul(self.surface.height as usize)
            .min(64 * 1024);
```
- Confidence: Read
- Impact: `WaylandDmabufCapturer::capture_next_frame` (`linux_capture.rs:299`, `0x80`), `X11ShmCapturer::capture_next_frame` (`linux_capture.rs:425`, `0x40`), `WindowsDxgiCapturer::capture_next_frame` (`windows.rs:587`, `0x7F`), and `MacOsScreenCaptureKitCapturer::capture_next_frame` (`macos.rs:383`, `0x90`) never read GPU surfaces or screen memory; even when `is_live_display()` is `true`, each allocates a `64 KiB` constant buffer (less than 7 scanlines of a `2560x1440` BGRA8 frame).
- Direction: Implement real frame acquisition per backend (Wayland PipeWire/Screencopy DMA-BUF, X11 `shm::get_image`, Windows `IDXGIOutputDuplication::AcquireNextFrame`, macOS `SCStreamOutput` `CMSampleBuffer` -> `IOSurface`) and pass zero-copy GPU handles or full-frame planes to the encoder.

### CAP-02: Platform capture probes immediately drop their OS connections and COM handles without retaining capture state, and omit required Linux crates
- Severity: High
- Category: stub
- Requirement: R12
- Evidence: `crates/rferram-server/src/backends/windows.rs:468-475`
```rust
                            d3d_device.and_then(|device| {
                                let output1: IDXGIOutput1 = output.cast().ok()?;
                                let dupl: IDXGIOutputDuplication =
```
- Confidence: Read
- Impact: `WindowsDxgiCapturer::probe_windows_dxgi_output` (`windows.rs:445-480`) creates `ID3D11Device` and `IDXGIOutputDuplication` only to read `dupl.GetDesc()` and drops every COM interface before returning; similarly, `WaylandDmabufCapturer::probe_wayland_environment` (`linux_capture.rs:244`) and `X11ShmCapturer::probe_x11_display` (`linux_capture.rs:362-373`) drop their `wayland_client::Connection` and `x11rb` connection immediately, `MacOsScreenCaptureKitCapturer` (`macos.rs:294-295`) only calls `AnyClass::get`, and `crates/rferram-server/Cargo.toml:27-35` omits `gbm`, `drm`, `calloop`, `smithay-client-toolkit`, `input`, `libseat`, and `udev` despite root `Cargo.toml:57-68` and `.agents/skills/rferram-capture-and-compositor/SKILL.md:24`.
- Direction: Store the live Wayland/PipeWire, X11 SHM, D3D11/DXGI, and `SCStream` handles inside each capturer struct and wire the required platform dependencies in `crates/rferram-server/Cargo.toml`.

### CAP-03: Display geometry is hardcoded to `2560x1440`, `enumerate_monitors()` and `WireMessage::Topology` are unused, and probes clamp small screens upward
- Severity: High
- Category: gap
- Requirement: R6
- Evidence: `crates/rferram-server/src/lib.rs:106-119`
```rust
            CaptureBackendType::WaylandPipeWire | CaptureBackendType::X11Fallback => {
                LinuxInputController::new(backend, 2560, 1440).ok()
            }
```
- Confidence: Read
- Impact: `NativeInputInjector::new` (`lib.rs:107-118`), `EncoderConfig::default()` (`encoder.rs:26-27`), and `HostSessionEngine` (`daemon.rs:79-92, 166-168, 595-598`) hardcode `2560x1440`; `X11ShmCapturer::probe_x11_display` (`linux_capture.rs:365-366`) computes `u32::from(screen.width_in_pixels).max(fallback_width)`, inflating a `1920x1080` screen to `2560x1440`; `FrameCapturer::enumerate_monitors()` is never called outside unit tests; and `TopologyMessage::DisplayLayout` / `ResizeRequest` (`crates/rferram-protocol/src/lib.rs:199-215`) are never sent or handled.
- Direction: Query actual monitor geometry from the active capturer, send `TopologyMessage::DisplayLayout` on session startup and display change, and parameterize both the encoder and `NativeInputInjector` with the real monitor bounds and origin.

### CAP-04: Docstrings across `rferram-server` misrepresent unconditional synthetic stubs as "headless CI fallbacks"
- Severity: Low
- Category: hygiene
- Requirement: R6
- Evidence: `crates/rferram-server/src/backends/linux_capture.rs:196-198`
```rust
    /// Initializes the Wayland DMABUF/GBM frame capturer, probing the ambient Wayland socket on Linux
    /// and falling back cleanly to a synthetic DMA-BUF surface in headless CI environments.
    pub fn new(display_id: u32, width: u32, height: u32) -> Result<Self> {
```
- Confidence: Read
- Impact: Module and constructor docstrings in `lib.rs:3-12`, `linux_capture.rs:1-5, 196-197, 317-318`, `linux_input.rs:1-5, 435-437, 714-715`, `windows.rs:1-9, 349-351`, and `macos.rs:1-8, 223-225` state that synthetic buffers are used only as a fallback in headless CI environments, whereas in reality no live capture or Linux input code path exists even when a physical display server is connected.
- Direction: Update module and item docstrings to accurately state current capabilities or replace the stubs with real backend implementations.

### ENC-01: Video encoders only copy the first `1024` or `2048` raw bytes of `frame.data`, and `rav1e` is not a dependency of `rferram-server`
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-server/src/encoder.rs:153-155`
```rust
        let payload_size = std::cmp::min(1024, frame.data.len());
        let encoded_payload = frame.data[..payload_size].to_vec();
```
- Confidence: Read
- Impact: `SoftwareVideoEncoder::encode` (`encoder.rs:153-154`) copies the first `1024` bytes of `frame.data` and `MacOsVideoToolboxEncoder::encode` (`encoder.rs:83-84`) copies the first `2048` bytes without running any video codec, while `crates/rferram-server/Cargo.toml:15-26` omits `rav1e` (and declares an unused `wgpu` dependency at `:25`).
- Direction: Add `rav1e` (and platform hardware encoder bindings: VAAPI/Vulkan Video, NVENC/MediaFoundation, VideoToolbox `VTCompressionSessionRef`) to `rferram-server` and encode real bitstreams in `VideoEncoder::encode`.

### ENC-02: Keyframe, bitrate, framerate, and quality control messages have no effect on encoded payloads or capture cadence
- Severity: Medium
- Category: gap
- Requirement: R6
- Evidence: `crates/rferram-server/src/daemon.rs:146-148`
```rust
        tokio::spawn(async move {
            let target_frame_duration = Duration::from_nanos(1_000_000_000 / 60);
```
- Confidence: Read
- Impact: Although `encoder.rs:77-81, 147-151` and `daemon.rs:228-234` flip `is_keyframe = true` on `frame_counter == 1`, `LossReport`, and multiples of `keyframe_interval` (satisfying the boolean check in `.agents/skills/rferram-capture-and-compositor/SKILL.md:37`), the emitted payload is identical to an inter-frame; `update_bitrate` (`encoder.rs:100-107, 170-173`) and `update_framerate` (`:109-116, 175-178`) only mutate unused config fields; `run_capture_loop` (`daemon.rs:147`) hardcodes a 60 FPS sleep timer; and `ControlMessage::SetTargetFps` / `ControlMessage::SetQuality` (`crates/rferram-protocol/src/lib.rs:392-396`) are ignored by the `_ => {}` wildcard in `daemon.rs:256`.
- Direction: Wire `EncoderConfig::{target_bitrate_kbps, target_fps}` and `force_keyframe` into a real encoder rate-control and intra-frame refresh state machine, drive the capture timer from `target_fps`, and handle `SetTargetFps` and `SetQuality` in `HostSessionEngine::handle_incoming_wire_message`.

### AUD-01: `HostAudioCapturer` emits zero-filled 192-byte buffers every 20 ms with no OS audio loopback capture or Opus encoding
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-server/src/audio.rs:51-60`
```rust
    pub fn build_audio_frame(stream_id: u32, timestamp_us: u64) -> AudioFrameMessage {
        AudioFrameMessage {
...
            payload: vec![0u8; OPUS_PAYLOAD_BYTES],
```
- Confidence: Read
- Impact: `start_loopback_capture` (`audio.rs:64-94`) spawns a 20 ms timer loop that sends `vec![0u8; 192]` tagged as `AudioCodec::Opus` (`48 kHz` stereo); no OS audio capture (`PipeWire`, Windows `WASAPI`, macOS `CoreAudio`/`ScreenCaptureKit`) or Opus encoder exists in the workspace, and 192 zero bytes is not a valid stereo 20 ms Opus packet.
- Direction: Integrate per-OS loopback audio capture (`PipeWire` stream on Linux, `IAudioClient` `AUDCLNT_STREAMFLAGS_LOOPBACK` on Windows, `SCStreamOutputType::Audio` / `CoreAudio` tap on macOS) feeding a real Opus encoder.

### IN-01: Linux Wayland and X11 input injectors only update in-memory state and log `debug!` without injecting OS input
- Severity: High
- Category: stub
- Requirement: R2
- Evidence: `crates/rferram-server/src/backends/linux_input.rs:566-576`
```rust
                let coords = self.update_pointer(*x, *y, *is_absolute)?;
                debug!(
                    monitor_id,
```
- Confidence: Read
- Impact: `WaylandVirtualInputInjector::inject_input` (`linux_input.rs:557-700`) and `X11XTestInputInjector::inject_input` (`linux_input.rs:864-982`) drop their display connections during `new()` (`:510, 782`), never bind `zwlr_virtual_pointer_manager_v1` / `zwp_virtual_keyboard_manager_v1` or call `xtest_fake_input`, and only mutate local fields (`last_pointer_coords`, `last_evdev_scancode`, `last_x11_keycode`, `injected_count`) and emit `debug!` logs.
- Direction: Retain a live Wayland virtual-input / portal `RemoteDesktop` / `/dev/uinput` handle and a persistent `x11rb` connection calling `xtest_fake_input` + `flush()` in `LinuxInputController`.

### IN-02: Windows `SendInput` and macOS `CGEventPost` have mismatched coordinate spaces, incomplete scancode tables, broken relative/Unicode events, and no privilege/TCC handling
- Severity: High
- Category: gap
- Requirement: R12
- Evidence: `crates/rferram-server/src/backends/macos.rs:497-505`
```rust
        InputEvent::MouseMove {
            x, y, is_absolute, ..
        } => {
            let (clamped_x, clamped_y) = if *is_absolute {
                (x.clamp(0.0, max_x), y.clamp(0.0, max_y))
            } else {
                (*x, *y)
            };
```
- Confidence: Read
- Impact: While `WindowsSendInputInjector` (`windows.rs:1048`) and `MacOsCgEventInjector` (`macos.rs:728-794`) call real OS input APIs, (1) both treat `is_absolute: true` coordinates as pixel values (`0..screen_width`) whereas `linux_input.rs:161-166` treats `[0.0, 1.0]` as normalized ratios; (2) on macOS, `is_absolute: false` passes raw relative deltas `(*x, *y)` to `CGEventCreateMouseEvent` (`macos.rs:503-510`), warping the cursor to absolute `(dx, dy)`, and `UnicodeText` (`:787-797`) posts `key_down = true` with no key-up; (3) `map_scancode_to_win32_vk` (`windows.rs:661-682`) maps only 19 scancodes and `map_scancode_to_macos_keycode` (`macos.rs:470-484`) maps only 12 scancodes before falling back to `(key_code & 0xFFFF) as u16`; and (4) neither backend checks macOS TCC Accessibility (`AXIsProcessTrusted`) or switches Windows thread desktops (`OpenInputDesktop`/`SetThreadDesktop`) for UAC/Winlogon.
- Direction: Standardize the wire coordinate convention across all three OS backends, complete the USB HID / `evdev` scancode lookup tables for Win32 `VK_*`/Set-1 and macOS `kVK_*`, fix macOS relative mouse delta and Unicode key-up events, and add macOS TCC preflight checks and Windows desktop-switch handling.

## Open questions for Claude
1. For Requirement R6, should Phase 2+ target **screen share** (mirroring the physical desktop via `xdg-desktop-portal` + `PipeWire` on Linux, `IDXGIOutputDuplication` on Windows, `ScreenCaptureKit` on macOS) first, or **headless virtual display** first?
2. Should software AV1 encoding use `rav1e` (already in root `Cargo.toml:94`), or should `rferram-server` prioritize hardware encoding (`VAAPI`/`NVENC`/`VideoToolbox`/`MediaFoundation`) with a lighter low-latency codec fallback?
3. Is bidirectional or host-to-client audio loopback required for the initial milestone (note that `R1`–`R12` do not explicitly list audio, although `rferram-protocol` and `rferram-server/src/audio.rs` define `AudioFrameMessage`), or should `audio.rs` be deferred/removed until video + input are real?
