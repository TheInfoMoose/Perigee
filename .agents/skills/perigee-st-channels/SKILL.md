---
name: perigee-st-channels
description: >-
  Procedures and invariants for implementing and modifying perigee-channels
  (arboard cross-platform clipboard with loopback echo suppression; legacy OOB SSH,
  snapshot transfer, elevation, and passthrough removed in Phase 2 to be replaced
  by simply-transfer-link).
---

# Perigee Virtual Channels Skill (`perigee-channels`)

Activate this skill when working on `crates/perigee-channels`.

## 1. Clipboard Synchronization & Echo Suppression (`ClipboardChannelHandler`)

- **Headless / Display Resilience**:
  - `arboard::Clipboard::new()` can fail in headless CI environments or Wayland sessions without an active display server. Store `Option<Clipboard>` inside `Arc<Mutex<Option<Clipboard>>>` and log a `debug!` message on initialization failure so unit tests never panic in headless runners.
- **Two-Layer Loopback Suppression**:
  1. `is_suppressed: Arc<AtomicBool>`: Set to `true` (`Ordering::SeqCst`) before writing remote clipboard contents to the local OS pasteboard, and reset after a short settling window (`100ms`). The polling loop must skip iterations while `is_suppressed` is `true`.
  2. `last_sent_content: Arc<Mutex<ClipboardContent>>`: Track the last text string, `ImageHash(u64)`, or file list written or sent so identical consecutive payloads are never re-broadcast across QUIC.
- **Image Dimensions**:
  - Preserve actual image `width` and `height` in `ClipboardPayload::Image { width, height, format, data }` when reading/writing `arboard::ImageData`.

## 2. Subsystems Removed in Phase 2

- **Snapshot File Transfers (`snapshot_transfer.rs`)**: Removed in Phase 2; replaced by `simply-transfer-link` in Phase 6.
- **Embedded `russh` P2P OOB Key Ingestion (`oob_ssh.rs`)**: Removed in Phase 2; replaced by `simply-transfer-link` in Phase 4.
- **Session Elevation (`elevation.rs`) & Device Passthrough (`passthrough.rs`)**: Removed in Phase 2 until in scope.

## 3. Verification
Run the quality gate for `perigee-channels`:
```bash
.agents/skills/perigee-quality-gate/scripts/verify-crate.sh perigee-channels
```
