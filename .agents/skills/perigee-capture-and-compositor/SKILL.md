---
name: perigee-capture-and-compositor
description: >-
  Procedures and invariants for implementing and modifying perigee-host and
  perigee-client (Wayland/Smithay DMABUF capture and virtual input, Windows DXGI
  and SendInput, macOS objc2 ScreenCaptureKit/VideoToolbox/CGEvent, rav1e AV1
  encoding, wgpu 30.0.1 WGSL rendering, and latency feedback loops).
---

# Perigee Host Capture, Compositor & Client Render Pipeline Skill

Activate this skill when working on `crates/perigee-host` or `crates/perigee-client`.

## 1. Upstream Reference Projects

When inspecting upstream Smithay Wayland compositor APIs, DRM/GBM buffer handling, or virtual input protocols, consult the upstream repositories:
- **Smithay**: https://github.com/Smithay/smithay
- **COSMIC Compositor (`cosmic-comp`)**: https://github.com/pop-os/cosmic-comp

## 2. Locked Platform Dependencies (`Cargo.toml`)

Because `--locked` is mandatory, only use the platform crates declared in `Cargo.toml`:
- **Linux (`#[cfg(target_os = "linux")]`)**:
  - `smithay-client-toolkit = "0.19.2"`, `wayland-client = "0.31.15"`, `wayland-backend = "0.3.17"`, `wayland-protocols = "0.32.13"`, `wayland-protocols-wlr = "0.3.12"`, `calloop = "0.14.5"`, `drm = "0.14.1"`, `drm-fourcc = "2.2.0"`, `gbm = "0.18.0"`, `input = "0.10.0"`, `libseat = "0.2.4"`, `udev = "0.9.3"`, `xkbcommon = "0.9.0"`, `x11rb = "0.13.2"`, `zbus = "5.19.0"`.
- **Windows (`#[cfg(target_os = "windows")]`)**:
  - `windows = "0.62.2"` (`Win32_Foundation`, `Win32_Graphics_Dxgi`, `Win32_Graphics_Dxgi_Common`, `Win32_Graphics_Direct3D11`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_Media_Audio`, `Win32_Networking_WinSock`), `windows-sys = "0.61.2"`.
- **macOS (`#[cfg(target_os = "macos")]`)**:
  - `objc2 = "0.6.5"`, `objc2-foundation = "0.3.2"`, `objc2-core-foundation = "0.3.2"`, `objc2-core-graphics = "0.3.2"`, `objc2-io-surface = "0.3.2"`, `objc2-metal = "0.3.2"`, `objc2-quartz-core = "0.3.2"`.
- **Encoding & GPU Presentation**:
  - `rav1e = "0.8.1"`, `wgpu = "30.0.1"` (features: `vulkan`, `metal`, `dx12`, `wgsl`), `bytemuck = "1.25.2"`.

## 3. Architectural Invariants

1. **Cross-Platform Compilation Safety**:
   - Guard all OS-specific imports and FFI blocks with `#[cfg(target_os = "linux")]`, `#[cfg(target_os = "macos")]`, or `#[cfg(target_os = "windows")]`, while keeping trait definitions (`VideoEncoder`, `HostInputInjector`) and `CaptureBackendType` unconditional so cross-platform unit tests compile on every host OS.
2. **Keyframe & Bitrate Feedback Loop**:
   - `VideoEncoder::encode` must always emit a keyframe (`is_keyframe = true`) on `frame_counter == 1`, when `force_keyframe == true` (triggered by `ControlMessage::LossReport`), or when `frame_counter.is_multiple_of(keyframe_interval as u64)`.
   - `ClientSessionPipeline::demux_loop` must track `expected_frame_id`, immediately emit `ControlMessage::LossReport { missing_frames }` on sequence gaps, and emit `ControlMessage::FrameAck` with microsecond `render_delay_us` after forwarding each frame.
3. **FFI Safety Annotations**:
   - Every `unsafe` call to Win32 `SendInput`/`IDXGIOutputDuplication`, macOS `CGEvent`/`IOSurface`, or Linux DRM/X11 must include a `// SAFETY:` block.

## 4. Verification
Run the quality gate for `perigee-host` and `perigee-client`:
```bash
.agents/skills/perigee-quality-gate/scripts/verify-crate.sh perigee-host
.agents/skills/perigee-quality-gate/scripts/verify-crate.sh perigee-client
```
