# Phase 7 review

Range: `a8c07b82da28df7b067657ce2db35f2af8920618..9557e0158b18002ddfab7ea28eb1b50d8b8aeb92`

Changed code files:
- `crates/perigee-host/src/backends/wayland_shm.rs`
- `crates/perigee-host/src/backends/worker.rs`
- `crates/perigee-host/src/daemon.rs`
- `packaging/linux/perigee-host.service`

## Independent pass

### Findings

1. **`crates/perigee-host/src/backends/worker.rs:192-193` (and sibling `206-207`)** — `regression`
   - **Trigger:** A capture worker fails to exit within `WORKER_EXIT_TIMEOUT` (2 s) (or `child.try_wait()` returns `Err`) when the parent process lacks permission to signal the worker (e.g., `EPERM` if `perigee-host` is started with `CAP_SETUID` but without `CAP_KILL`, such as an un-updated installed service unit or non-systemd launch, after switching the worker to another UID).
   - **Effect:** `let _ = child.kill();` silently discards the error and unconditionally calls blocking `let _ = child.wait();`, hanging the calling thread indefinitely on a still-running worker. At `<base>`, `wait_for_worker_exit` returned after `WORKER_EXIT_TIMEOUT` without blocking forever. If `child.kill()` fails (other than the child already having exited), it should log the failure and return without calling blocking `child.wait()`.

2. **`crates/perigee-host/src/daemon.rs:328-336`** — `new defect`
   - **Trigger:** `video_tx.try_send(WireMessage::Video(video_msg))` returns `TrySendError::Full` on an encoder-initiated keyframe (`need_keyframe == false`, `video_msg.is_keyframe == true` — e.g., frame 1 or a periodic `keyframe_interval` keyframe), or on any delta frame once an inter-frame codec replaces `RawVideoEncoder`.
   - **Effect:** `if need_keyframe` is false, so `force_keyframe` is not re-armed; the client misses the initial/periodic keyframe until the next `keyframe_interval` or explicit client request. (Current `RawVideoEncoder` marks every frame `is_keyframe: true` and has no inter-frame state, so live raw sessions are unaffected today; `SoftwareVideoEncoder` / `MacOsVideoToolboxEncoder` and future real codecs are affected.)

3. **`crates/perigee-host/src/daemon.rs:693-700`** — `regression`
   - **Trigger:** `outgoing_tx` (now held solely by `HostAudioCapturer`) is dropped independently while `session.is_running()` is still `true` and `video_rx` is still active.
   - **Effect:** `outgoing_rx.recv()` returns `None`, causing `let Some(msg) = maybe_msg else { break; };` to exit the outbound QUIC pump, drop `video_rx`, and terminate the video capture loop (`Outgoing transport channel closed`). At `<base>`, `outgoing_tx` was shared between video and audio, so audio stopping alone could not close the pump channel. (Latent today because `audio_capturer.stop()` is only called from `HostSessionEngine::stop()`.)

4. **`crates/perigee-host/src/daemon.rs:1599-1660`** — `test defect`
   - **Trigger:** `test_video_backlog_is_bounded_and_drops_excess_frames` only exercises `HostSessionEngine::run_capture_loop_with_video` directly.
   - **Effect:** Neither `force_keyframe` preservation across a `TrySendError::Full` drop nor `PerigeeHostDaemon`'s dual-channel `tokio::select!` outbound pump (`outgoing_rx` + `video_rx`) is tested.

### Traced and clean
- `crates/perigee-host/src/backends/wayland_shm.rs`: `wait_fd_events_or_cancelled` generalized `POLLIN`/`POLLOUT` polling, `dispatch_or_cancelled` `WouldBlock` retry loop on `queue.flush()`, and `test_wait_fd_events_or_cancelled_pollout_and_cancel`.
- `crates/perigee-host/src/backends/worker.rs`: `should_warn_poll_failure` power-of-two backoff with reset on `poll_seat()` success, `test_poll_failure_backoff_resets_on_success`, and tightened `1000 ms` timeout in `test_worker_error_reaps_worker_blocked_on_write`.
- `packaging/linux/perigee-host.service`: `CAP_KILL` added to both `AmbientCapabilities` and `CapabilityBoundingSet`.
- Sensitive data check across all 4 diffs: clean.
- Test run: `cargo test -p perigee-host --all-targets --locked` — 73 passed, 0 failed.

### Not checked
- Live Wayland compositor capture, `systemd` service runtime with `CAP_KILL`, and real QUIC/WAN frame timing.
- Non-Linux capture backends and other workspace crates (unchanged in this range).
- UI layout (no UI files changed).

<!-- SEALED: Independent pass locked -->

## Contract pass

### Step compliance
- **STEP 7A (`TASK-0118`, `BUG-0021`, `BUG-0022`):** All contract items met (`CAP_KILL` in `AmbientCapabilities` and `CapabilityBoundingSet` with updated comment; `wait_for_worker_exit` polite wait then `kill()` + `wait()` on timeout/error; doc comments updated; `test_worker_error_reaps_worker_blocked_on_write` tightened to `1000 ms`; `test_wait_for_worker_exit_kills_and_reaps_hung_worker` passes).
- **STEP 7B (`TASK-0119`, `BUG-0023`):** All contract items met (`should_warn_poll_failure` power-of-two helper; `consecutive_poll_failures` tracking with reset on `Ok(())`, `saturating_add(1)` on `Err`, `warn!` vs `debug!`; `test_poll_failure_backoff_resets_on_success` passes).
- **STEP 7C (`TASK-0120`, `BUG-0024`):** All contract items met (`wait_fd_events_or_cancelled` extracted, `wait_readable_or_cancelled` preserved; `dispatch_or_cancelled` `WouldBlock` retry loop on `POLLOUT`; `test_wait_fd_events_or_cancelled_pollout_and_cancel` passes).
- **STEP 7D (`TASK-0121`, `BUG-0019`):** All contract items met (`MAX_OUTBOUND_VIDEO_BACKLOG = 2`; `run_capture_loop_with_video` + delegating `run_capture_loop`; `try_send` drop + `enqueue_ms`; dual-channel `tokio::select!` in `handle_incoming_connection` + `net_send_ms` log; `test_video_backlog_is_bounded_and_drops_excess_frames` passes).

### Unrequested changes / validator deviations / deferrals / amendments
- **Unrequested changes:** none.
- **Validator deviations:** none.
- **Deferrals:** none in Phase 7.
- **Amendments:** none in Phase 7.

### Finding origins & tracker IDs
| # | File | Class | Origin | Severity | Filed ID |
| :-: | :--- | :--- | :--- | :--- | :--- |
| 1 | `crates/perigee-host/src/backends/worker.rs:192-193, 206-207` | `regression` | `plan` (contract explicitly wrote `let _ = child.kill(); let _ = child.wait();`) | `medium` | `BUG-0025` |
| 2 | `crates/perigee-host/src/daemon.rs:328-336` | `new defect` | `plan` (contract specified checking `need_keyframe` only) | `low` | `BUG-0026` |
| 3 | `crates/perigee-host/src/daemon.rs:693-700` | `regression` | `coder` (`select!` treats either channel returning `None` as pump exit) | `low` | `BUG-0027` |
| 4 | `crates/perigee-host/src/daemon.rs:1599-1660` | `test defect` | `plan` (contract `Tests:` covered only `run_capture_loop_with_video`) | `low` | `TASK-0122` |

### Advances items ready to close (pending user approval)
- `BUG-0019`, `BUG-0021`, `BUG-0022`, `BUG-0023`, `BUG-0024` (all 5 code contracts met and unit-tested; `[USER] 5BI` remains for live service verification).

## Handoff

- **Verdict:** PASS WITH FOLLOW-UPS (all 4 code steps 7A–7D met contract; no deferrals or amendments; `[USER] 5BI` remains before Phase 8).
- **Medium:**
  - `BUG-0025` (`perigee_host`, origin: `plan`): `wait_for_worker_exit` discards `child.kill()` errors and blocks forever in `child.wait()` on `EPERM` (e.g., missing `CAP_KILL`).
- **Low:**
  - `BUG-0026` (`perigee_host`, origin: `plan`): `TrySendError::Full` only re-arms `force_keyframe` when `need_keyframe` was true (misses encoder-initiated keyframes and future inter-frame AV1 reference chains).
  - `BUG-0027` (`perigee_host`, origin: `coder`): `outgoing_rx` closing independently terminates the outbound QUIC pump for video.
  - `TASK-0122` (`perigee_host`, origin: `plan`): add tests for `force_keyframe` preservation on drop and `PerigeeHostDaemon` dual-channel pump.
- **Deferred steps to re-split:** none.
- **Ready to close (pending user approval):** `BUG-0019`, `BUG-0021`, `BUG-0022`, `BUG-0023`, `BUG-0024`.
- **Decisions for user:**
  1. Run `[USER] 5BI` (live service test with frame timing) before planning Phase 8 (VA-API AV1).
  2. Decide whether to fold `BUG-0025`–`BUG-0027` and `TASK-0122` into early Phase 8 (especially `BUG-0026` before AV1 encoding).
