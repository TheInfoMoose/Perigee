# Phase 9b review

Range: `14fb9b334349dd985c45b1209f8a90ce240af8ea..e75ef460a3e4180b11f0fafd5a42859c7d8a39df`

## Independent pass

### Findings

1. **`crates/perigee-host/src/backends/uinput.rs:263-274,329-339`** (`regression`)
   - **Trigger:** Any multi-monitor session (`monitors.len() > 1`) where `screen_sink` (`crates/perigee-host/src/daemon.rs:473-478`) calls `UinputInjector::set_screen_size(video_msg.width, video_msg.height)` with a frame size smaller than the monitor's physical resolution. This occurs on the first frame for every monitor >= 1080p under `--codec raw` (default, because `RawVideoEncoder` calls `fit_raw_frame` to `RAW_FRAME_MAX_BYTES` = 4 MiB: `1920x1080` -> `960x540`, `2560x1440` -> `1280x720`, `3840x2160` -> `1280x720`), and for every monitor >= 1080p under `--media follow-seat` with **any** codec (because `worker_frame_message` in `crates/perigee-host/src/host.rs:615` also decimates via `fit_raw_frame`). It also triggers if `WorkerCapturer` repeats its `64x64` initial standby frame after a worker sends `DisplayLayout` and exits before sending a video frame.
   - **Effect:** `set_screen_size` invokes `OutputLayout::update_active_size`, overwriting the active monitor's `width`/`height` in `output_layout` with the decimated frame size while leaving `scale_factor` and sibling monitors' logical `(x, y)` origins unchanged. `logical_monitor_size` (`uinput.rs:142-154`) and `compute_virtual_desktop_bounds` (`uinput.rs:159-194`) then shrink the active monitor's logical footprint (e.g., `1920x1080 @ 1.0x` -> `960x540`; `3840x2160 @ 2.0x` -> `1280/2 x 720/2 = 640x360`), introducing a dead gap before sibling monitors and making the right and bottom portions of the active monitor unreachable even with normalized `[0.0, 1.0]` pointer coordinates. At `<base>`, `set_screen_size` did not mutate `output_layout`, so normalized multi-monitor pointer mapping under `--codec raw` was unaffected.
   - **Cause:** `MonitorInfo.width`/`.height` in `output_layout` is conflated between compositor physical output dimensions (required for logical geometry and virtual desktop bounds) and encoded frame pixel dimensions (`screen_width`/`screen_height`, required only to normalize pixel-space pointer inputs).

2. **`crates/perigee-host/src/backends/uinput.rs:343-357`** (`new defect`)
   - **Trigger:** Calling `UinputInjector::set_output_layout(monitors, active_monitor_id)` (e.g. from `WorkerCapturer`'s `layout_sink` on a capture thread while `set_screen_size` runs on the encode/send task).
   - **Effect:** `set_output_layout` calls `self.set_screen_size(active.width, active.height)` before acquiring `self.output_layout.lock()` to install `monitors`, which acquires the mutex twice, mutates the stale layout on the first lock, and leaves a race window where `screen_width`/`screen_height` and `output_layout` can diverge under concurrent updates.

3. **`crates/perigee-host/src/backends/uinput.rs:631-657`** (`test defect`)
   - **Trigger:** `test_output_layout_update_active_size_syncs_active_monitor` mutates `OutputLayout` in isolation and checks struct fields without exercising `compute_virtual_desktop_bounds` or `map_output_pointer_to_abs` after a decimated `screen_sink` update.
   - **Effect:** The test passes even though `update_active_size` corrupts multi-monitor pointer mapping on every >= 1080p `--codec raw` and `--media follow-seat` session.

### What was traced
- Full diff of all 5 changed code files: `crates/perigee-host/src/backends/mod.rs`, `crates/perigee-host/src/backends/uinput.rs`, `crates/perigee-host/src/backends/worker.rs`, `crates/perigee-host/src/daemon.rs`, `crates/perigee-host/src/host.rs`.
- `logical_monitor_size`, `compute_virtual_desktop_bounds`, `map_output_pointer_to_abs`, `OutputLayout::update_active_size`, `UinputInjector::set_screen_size`, `UinputInjector::set_output_layout`, and `UinputInjector::inject_input` against `wayland_shm.rs::output_info_to_monitor_info` and `encoder.rs::fit_raw_frame`.
- `WorkerCommand::build_with_mode`, `read_worker_message`, `read_worker_frame`, `WorkerCapturer::with_capture_mode`, `WorkerCapturer::with_layout_sink`, `WorkerCapturer::capture_next_frame`, `WorkerCapturer::enumerate_monitors`, `WorkerCapturer::stop_worker`, and `HostTool::CaptureWorker` layout emission.
- `HostOptions::capture_mode`, `PERIGEE_CAPTURE_MODE`, `--capture-mode`, `PerigeeHostDaemon::with_capture_mode`, `HostSessionEngine::build_live_media`, and `HostSessionEngine::build_follow_seat_media`.
- Sensitive data scan across all 5 changed files (none found).
- `cargo test -p perigee-host --all-targets --locked` (105 passed, 0 failed).

### What was not checked
- Live Wayland compositor / DRM DMA-BUF / `/dev/uinput` hardware execution (not available in this review environment).
- Live `systemd-logind` multi-session `follow-seat` switching with `setgroups(0, NULL)` against `/dev/dri/renderD*` permissions.

<!-- SEALED: Independent pass locked -->

## Contract pass

### Step compliance
- **STEP 9BA (`TASK-0144` — `BUG-0040`):** All contract items and both required tests were implemented as specified (`logical_monitor_size`, `compute_virtual_desktop_bounds`, `map_output_pointer_to_abs`, `OutputLayout::update_active_size`, `UinputInjector::set_screen_size`). No scope violations or unrequested changes. However, the plan's requirement to mutate `output_layout`'s active monitor dimensions inside `set_screen_size` caused a high-severity multi-monitor pointer regression (Finding 1) and exposed a double-lock/race window in `set_output_layout` (Finding 2).
- **STEP 9BB (`TASK-0145` — `BUG-0041`):** Met in full. `HostTool::CaptureWorker` emits `DisplayLayout` before the frame loop; `WorkerCapturer` decodes `WorkerMessage`, caches monitors, clears on `stop_worker`, returns cached monitors from `enumerate_monitors`, and forwards to `layout_sink`; `build_follow_seat_media` wires `layout_sink` to `UinputInjector::set_output_layout`. Both required tests pass (using `ControlMessage::KeepAlive`, the actual protocol variant). No scope violations.
- **STEP 9BC (`TASK-0146`):** Met in full. `HostOptions.capture_mode`, `PERIGEE_CAPTURE_MODE`, `--capture-mode`, `HOST_USAGE`, `WorkerCommand::build_with_mode`, `WorkerCapturer::with_capture_mode`, `PerigeeHostDaemon::with_capture_mode`, `HostSessionEngine::with_media_codec_output_and_capture_mode`, `build_live_media` (`WaylandCapturer::connect`), and `build_follow_seat_media` all match the contract. All 3 required tests pass. No scope violations.
- **Deferrals / Amendments:** None in Phase 9b.

### Independent findings — origin & tracking
| # | Location | Class | Origin | Severity | Filed item |
|---|----------|-------|--------|----------|------------|
| 1 | `crates/perigee-host/src/backends/uinput.rs:263-274,329-339` | `regression` | `plan` | high | `BUG-0042` |
| 2 | `crates/perigee-host/src/backends/uinput.rs:343-357` | `new defect` | `coder` | low | `BUG-0043` |
| 3 | `crates/perigee-host/src/backends/uinput.rs:631-657` | `test defect` | `plan` | high (tracked with #1) | `BUG-0042` |

### `Advances:` items
- `BUG-0040`: HiDPI logical/physical coordinate mixing is fixed by `logical_monitor_size` + `compute_virtual_desktop_bounds` + `map_output_pointer_to_abs`; the `set_screen_size` regression is separately tracked in `BUG-0042`. Ready to close on user approval (or keep open until `BUG-0042` lands).
- `BUG-0041`: Follow-seat `DisplayLayout` forwarding into `UinputInjector::set_output_layout` is complete and tested. Ready to close on user approval.

## Handoff

- **Verdict:** **Needs fix** — `STEP 9BB` and `STEP 9BC` are clean, and `STEP 9BA` fixed HiDPI logical/physical mixing (`BUG-0040`), but `STEP 9BA`'s `set_screen_size` -> `update_active_size` contract introduced a high-severity multi-monitor pointer regression on every >= 1080p `--codec raw` and `--media follow-seat` session.
- **Pending items filed (by priority):**
  - **High:** `BUG-0042` (`perigee_host`) — `UinputInjector::set_screen_size` overwrites `output_layout`'s active monitor `width`/`height` with `fit_raw_frame`-decimated frame dimensions, shrinking its logical footprint and making right/bottom regions unreachable; separate encoded frame size (`screen_width`/`screen_height`) from compositor physical geometry and add a regression test.
  - **Low:** `BUG-0043` (`perigee_host`) — `UinputInjector::set_output_layout` double-locks `output_layout` and leaves a race window between `screen_width`/`screen_height` and `output_layout` updates.
- **Deferred steps to re-split:** None (all 3 Phase 9b steps completed).
- **Decisions for the user:**
  - Approve closing `BUG-0040` (HiDPI mixing fixed; regression tracked in `BUG-0042`) and `BUG-0041` (follow-seat layout forwarding complete), or keep `BUG-0040` open until `BUG-0042` is fixed.
