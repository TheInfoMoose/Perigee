# Phase 8b review

Range: `233526e25ce4eedcd085521f38cf9bd5a4a2f83c..680c7a22c5eab4349608f5146971f2cc2fff2ff1`

## Independent pass

### Findings

1. **`crates/perigee-host/src/encoder_gst.rs:742-744`** — **test defect** (low)
   - **Trigger:** Running `test_gstreamer_av1_encoder_falls_back_to_software_and_recovers_after_error`.
   - **Effect:** The test manually sets `encoder.use_vaapi = true` and directly calls `encoder.fallback_to_software(...)` before calling `encoder.encode(&frame, true)`. It never exercises the runtime fallback branch in `VideoEncoder::encode` (`crates/perigee-host/src/encoder_gst.rs:404-407`). If that fallback match arm were deleted or broken, the test would still pass. Faulting an active pipeline (or leaving `active: None` when `vaav1enc` is unavailable) with `encoder.use_vaapi = true` and calling `encoder.encode(...)` would test the actual fallback path.

2. **`crates/perigee-client/src/decode.rs:559-565`** — **test defect** (low)
   - **Trigger:** Running `test_should_fallback_to_software_preserves_vaav1dec_on_corrupt_or_orphaned_frames`.
   - **Effect:** The test only asserts the pure boolean helper `should_fallback_to_software(...)` in isolation. It does not exercise `GstreamerAv1Decoder::decode`, `DecodeError` classification (`Pipeline` vs `TimeoutOrInput`), or `has_decoded_frame` state updates. If `decode()` stopped setting `self.has_decoded_frame = true` or misclassified a timeout as `DecodeError::Pipeline`, the test would still pass.

### Traced and verified clean
- `crates/perigee-cli/src/lib.rs`: `--window` + `--dump-frame` mutual exclusion in `parse_connect_args` and tests in both flag orders; RFC 5737 placeholder IP (`192.0.2.10`) only.
- `crates/perigee-cli/src/main.rs`: `tokio::task::block_in_place` wrapping `run_viewer_window` in `run_connect_window` under `#[tokio::main]` (`features = ["full"]`); `EventLoop::new()` early-error path drops `shutdown_tx`, which unblocks `lifecycle_task`.
- `crates/perigee-client/src/decode.rs`: `DecodeError` split, `ensure_pipeline` software fallback on `vaav1dec` build failure, `has_decoded_frame` gating so post-startup corrupt/orphaned frames do not downgrade `vaav1dec`.
- `crates/perigee-client/src/window.rs`: `classify_surface_recovery` and `GpuState::render` reconfiguring + calling `request_redraw()` on `Lost`/`Outdated` while skipping `Timeout`/`Occluded` and failing on `Validation`.
- `crates/perigee-host/src/daemon.rs`: `tokio::task::spawn_blocking` + `encoder.blocking_lock()` raced against `stop_rx.changed()` in `run_capture_loop_with_video`, with `BlockingEncoder` unit test verifying exit within 500 ms.
- `crates/perigee-host/src/encoder_gst.rs`: pipeline teardown on `push_buffer`/`try_pull_sample`/`sample.buffer()`/`map_readable()` failures, monotonic `next_pts_ns` across `update_framerate` increases, and `appsrc` caps update via `build_src_caps`.
- Static & test gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test -p perigee-cli -p perigee-client -p perigee-host --all-targets --locked` all exited 0.
- Sensitive data scan: no real hosts, IPs, emails, tokens, credentials, or internal paths introduced.

### Not checked
- Live VA-API hardware encode (`vaav1enc`) and decode (`vaav1dec`) on a GPU with AV1 VA-API support.
- Live Wayland/X11 `wgpu` window surface `Lost`/`Outdated` recovery and `perigee connect --window` interactive display session.
- Unchanged crates outside `perigee-cli`, `perigee-client`, and `perigee-host`.

<!-- SEALED: Independent pass locked -->

## Contract pass

### Step compliance (`8BA`–`8BE`)
- **STEP 8BA (`TASK-0132`, closed; advances `BUG-0029`, `BUG-0031`):** All contract items met (`encode_once` teardown on error, `fallback_to_software` + runtime `vaav1enc` -> `av1enc` retry in `encode`, `next_pts_ns` / `next_frame_pts_ns` monotonic PTS, `appsrc` caps update in `update_framerate`, and both required tests).
- **STEP 8BB (`TASK-0133`, closed; advances `BUG-0030`):** All contract items met (`has_decoded_frame`, `DecodeError::{Pipeline, TimeoutOrInput}`, `ensure_pipeline` fallback, `should_fallback_to_software`, `decode` fallback gating, and required test).
- **STEP 8BC (`TASK-0134`, closed; advances `BUG-0032`):** All contract items met (`spawn_blocking` + `encoder.blocking_lock()`, `tokio::select!` with `stop_rx.changed()`, `JoinError` stops session, `test_capture_loop_stops_while_encoder_blocks` verifies exit within 500 ms).
- **STEP 8BD (`TASK-0135`, closed; advances `BUG-0033`):** All contract items met (`--dump-frame` + `--window` rejected in `parse_connect_args`, `block_in_place` around `run_viewer_window`, and both flag orders tested).
- **STEP 8BE (`TASK-0136`, closed; advances `BUG-0034`):** All contract items met (`SurfaceRecoveryAction` + `classify_surface_recovery`, `Lost`/`Outdated` reconfigures + calls `request_redraw()`, `Timeout`/`Occluded` skip, `Validation` errors, and all 5 variants tested).

### Scope, deferrals, and amendments
- **Unrequested changes / scope lapses:** None. Every modified file matches its step's `ALLOWED_FILES`.
- **Deferrals:** None (0 deferred steps in Phase 8b).
- **Amendments:** None.

### Finding origins & tracker links
- **Finding 1 (`BUG-0035`, pending, low):** origin `plan` — `STEP 8BA`'s `Tests:` section explicitly directed calling `fallback_to_software(...)` directly (`STEPPED_IMPLEMENTATION_PLAN.md:580`), so the coder followed the plan, leaving the runtime fallback arm in `VideoEncoder::encode` unexercised by tests. Retained as a test defect.
- **Finding 2 (`BUG-0036`, pending, low):** origin `plan` — `STEP 8BB`'s `Tests:` section only specified testing the pure helper `should_fallback_to_software` (`STEPPED_IMPLEMENTATION_PLAN.md:638-643`), leaving `decode()`'s `has_decoded_frame` and `DecodeError` wiring unexercised. Retained as a test defect.

### Advances ready to close (after user approval)
- `BUG-0029`, `BUG-0031` (STEP 8BA)
- `BUG-0030` (STEP 8BB)
- `BUG-0032` (STEP 8BC)
- `BUG-0033` (STEP 8BD)
- `BUG-0034` (STEP 8BE)

## Handoff

- **Verdict:** PASS (all 5 steps `8BA`–`8BE` met contract; 0 regressions, 0 product-code defects, 2 low-severity test defects).
- **Ready to proceed to `[USER] 8H`:** Yes — live COSMIC service test (5BI) + VA-API AV1 + `perigee connect --window`.
- **New pending items (low priority, non-blocking for 8H):**
  - `BUG-0035` (`perigee_host`, low): test `GstreamerAv1Encoder` runtime `vaav1enc`->`av1enc` fallback through `VideoEncoder::encode`.
  - `BUG-0036` (`perigee_client`, low): test `GstreamerAv1Decoder::decode` `has_decoded_frame` and `DecodeError` fallback gating.
- **Deferred steps to re-split:** None.
- **Advances ready to close (pending user `approved`):** `BUG-0029`, `BUG-0030`, `BUG-0031`, `BUG-0032`, `BUG-0033`, `BUG-0034`.
- **Decisions for user:** Confirm closing `BUG-0029`–`BUG-0034` and whether to run `[USER] 8H` next before Phase 9 planning.
