# Phase 8 review

Range: `80a37df5bf58111fc4edc9f649e09ae3d7e062f2..940cc49bd620968e861f71f3f9f1d7986e13b730` (plus unstaged working-tree changes in `Cargo.toml`, `Cargo.lock`, `crates/perigee-client/Cargo.toml`, `crates/perigee-host/Cargo.toml`).

## Independent pass

### Findings

1. **`Cargo.toml`, `Cargo.lock`, `crates/perigee-host/Cargo.toml`, `crates/perigee-client/Cargo.toml` — Uncommitted dependency changes leave `HEAD` unbuildable** (`regression`)
   - **Trigger:** Checking out `HEAD` (`940cc49b`) cleanly without the unstaged working-tree modifications.
   - **Effect:** `crates/perigee-host/src/encoder_gst.rs`, `crates/perigee-client/src/decode.rs`, and `crates/perigee-client/src/window.rs` are committed at `HEAD` and import `gstreamer`, `gstreamer_app`, `gstreamer_video`, and `winit`, but the Cargo manifests and `Cargo.lock` declaring those dependencies are not committed. A clean checkout of `HEAD` fails to compile.

2. **`crates/perigee-host/src/encoder_gst.rs:70-92, 110-249, 331-350` — `GstreamerAv1Encoder` has no runtime fallback from `vaav1enc` to `av1enc` and leaves failed pipelines wedged** (`new defect`)
   - **Trigger:** Running with `enable_hardware_accel: true` (as `MediaMode::Live` and `MediaMode::FollowSeat` with `HostCodec::Av1` configure in `crates/perigee-host/src/daemon.rs:152`) on a host where `vaav1enc` is present in GStreamer's plugin registry (`ElementFactory::find("vaav1enc").is_some()`) but VA-API AV1 encoding fails at runtime (unsupported GPU, headless/VM, or `/dev/dri/renderD*` permission denied), or on any transient `push_buffer` / `try_pull_sample` failure.
   - **Effect:** Unlike `GstreamerAv1Decoder` (`crates/perigee-client/src/decode.rs:76-83, 345-348`), `GstreamerAv1Encoder` never falls back to software `av1enc` if `vaav1enc` fails to link, reach `Playing`, or encode. Additionally, `encode_once` does not call `self.tear_down_pipeline()` when `push_buffer` or `try_pull_sample` fails, leaving `self.active` stuck in a faulted state so every subsequent `encode()` fails until frame dimensions change.

3. **`crates/perigee-client/src/decode.rs:225-247, 343-349` — Any single corrupt or orphaned AV1 frame permanently disables hardware `vaav1dec` and stalls the client receive loop for 4 seconds** (`new defect`)
   - **Trigger:** Receiving a single corrupt AV1 payload or an orphaned delta frame (after packet loss before the next keyframe arrives) while `vaav1dec` is active.
   - **Effect:** `decode_once` blocks for `PULL_SAMPLE_TIMEOUT` (2 s), tears down the pipeline, then `decode()` treats every error as a hardware decoder failure, permanently switches `self.element_name = "av1dec"` for the rest of the session, and immediately retries the same bad frame on software `av1dec` (another 2 s timeout = 4 s total stall, directly observable in `test_session_pipeline_handles_corrupt_av1_without_crashing` taking ~4.07 s). While blocked inside `spawn_blocking(...).await` (`crates/perigee-client/src/session.rs:319-336`), `process_messages` processes no heartbeats, audio, or control messages.

4. **`crates/perigee-host/src/encoder_gst.rs:286-293, 419-430` — `update_framerate` causes backwards PTS jumps mid-stream** (`new defect`)
   - **Trigger:** Calling `GstreamerAv1Encoder::update_framerate(new_fps)` with a higher framerate than previously active (e.g., 30 -> 60 fps) after encoding frames.
   - **Effect:** `encode_once` computes `pts = (frame_idx + 1) * duration_ns` using the current `self.config.target_fps` rather than accumulating monotonic PTS. Increasing `target_fps` shrinks `duration_ns` and jumps PTS backwards (e.g., from ~3.37 s to ~1.70 s at frame 100), violating GStreamer's monotonic timestamp contract and risking dropped frames or rate-control stalls. `update_framerate` also does not update `appsrc` caps framerate.

5. **`crates/perigee-host/src/daemon.rs:378` — Blocking GStreamer AV1 encode runs directly on a Tokio async worker thread** (`new defect`)
   - **Trigger:** Any `HostCodec::Av1` session, especially during software `av1enc` encoding, forced-keyframe pipeline rebuilds, or 2 s `try_pull_sample` timeouts.
   - **Effect:** Unlike `capture_next_frame()` immediately above it (`daemon.rs:343-348`), which uses `tokio::task::spawn_blocking` and races against `stop_rx.changed()`, `encoder.lock().await.encode(&captured_frame, need_keyframe)` blocks the Tokio worker thread synchronously and prevents `stop_rx.changed()` from interrupting a hung encode until `encode()` returns.

6. **`crates/perigee-cli/src/main.rs:95-97, 281` — `--window` silently ignores `--dump-frame` and blocks a Tokio worker thread** (`new defect`)
   - **Trigger:** Running `perigee connect <host> --window --dump-frame out.ppm` (or running `--window` on a constrained/single-core runtime).
   - **Effect:** `run_connect` delegates to `run_connect_window` without passing `args.dump_frame`, silently ignoring `--dump-frame` with no error or warning. Additionally, `run_viewer_window` blocks the Tokio worker thread directly instead of using `tokio::task::block_in_place`.

7. **`crates/perigee-host/src/daemon.rs:69-82, 104-108` — `MediaMode::StandIn` + `HostCodec::Av1` silently clamps resolution to 128x128 and disables hardware accel** (`new defect`)
   - **Trigger:** Running `perigee-host --media standin --codec av1` or constructing `HostSessionEngine::with_media_and_codec` with `MediaMode::StandIn`, `HostCodec::Av1`, and dimensions > 128x128 or `enable_hardware_accel: true`.
   - **Effect:** Requested dimensions are silently clamped to `1..=128`, `enable_hardware_accel` is forced to `false`, and `X11Fallback` is silently remapped to `WaylandPipeWire`.

8. **`crates/perigee-client/src/window.rs:435-438` — Surface `Lost`/`Outdated` recovery does not request a redraw** (`new defect`)
   - **Trigger:** Swapchain reports `Lost` or `Outdated` (e.g., display reconfigure) while the remote stream is static.
   - **Effect:** `GpuState::render` reconfigures the surface and returns `Ok(())` without calling `self.window.request_redraw()`, leaving the window stale until the next incoming frame or OS redraw event.

### What was traced and checked
- All 15 code files changed in `80a37df5..940cc49b` were read in full via `git diff` / `read_file`.
- Verified `perigee-host/src/backends/worker.rs` `kill_and_reap_child` and grepped for sibling `.kill()` calls (none found).
- Verified `pump_outbound_channels` and keyframe re-arm on `TrySendError::Full` in `crates/perigee-host/src/daemon.rs`.
- Verified `compute_letterbox_viewport` and `format_window_title` edge cases (narrow, ultrawide, portrait, zero dimensions, long UTF-8 titles) in `crates/perigee-client/src/window.rs`.
- Verified `WgpuRenderPipeline` sRGB texture format alignment with `window.rs` surface format.
- Verified no sensitive data (real IPs, hosts, emails, tokens, internal paths) in changed files.
- Ran `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test -p perigee-host -p perigee-client -p perigee-cli --all-targets --locked` (all passed against the dirty working tree: 13 + 3 + 33 + 80 = 129 tests).

### What was NOT checked
- Live GUI rendering of `perigee connect --window` on a real Wayland/X11 compositor (`wgpu` surface presentation, live resize, Escape/Ctrl+Q interaction).
- Live VA-API hardware AV1 encode/decode (`vaav1enc` / `vaav1dec`) with real DRM render nodes and Wayland SHM / `follow-seat` capture.
- Non-Linux targets (Windows / macOS compilation and runtime).
- `perigee-ui` runtime behavior and `cargo test -p perigee-ui` (only `clippy --workspace --all-targets` was run on `perigee-ui`).

<!-- SEALED: Independent pass locked -->

## Contract pass

### Step-by-step contract evaluation
- **STEP 8A (`TASK-0123`, advances `BUG-0025`):** Met. `should_wait_after_kill` and `kill_and_reap_child` guard `child.wait()` in `crates/perigee-host/src/backends/worker.rs`; doc comment updated; `test_should_wait_after_kill_skips_wait_on_permission_denied` passes.
- **STEP 8B (`TASK-0124`, advances `BUG-0026`, `BUG-0027`, `TASK-0122`):** Met. `TrySendError::Full` re-arms `force_keyframe` on `need_keyframe || frame.is_keyframe || frame.codec != VideoCodec::RawBgra`; `pump_outbound_channels` tracks `outgoing_open` and `video_open` independently; both regression tests pass.
- **STEP 8C (`TASK-0125`):** Contract met in `encoder_gst.rs`, `encoder.rs`, `lib.rs`, but `[USER] 8PRE` manifest/lockfile changes were left uncommitted in the working tree (Finding #1). Runtime VA-API fallback and error teardown are missing (Finding #2), and `update_framerate` can jump PTS backwards (Finding #4).
- **STEP 8D (`TASK-0126`, advances `TASK-0103`):** Met. `HostCodec`, `--codec`, `PERIGEE_CODEC`, `LIVE_AV1_FPS`, `PerigeeHostDaemon::with_codec`, `HostSessionEngine::with_media_and_codec`, and both tests pass. Keep `TASK-0103` open until User Step 8H verifies live follow-seat AV1 framerate/latency.
- **STEP 8E (`TASK-0127`):** Met. `VideoDecoder`, `GstreamerAv1Decoder`, `ClientVideoDecoder`, `ClientSessionPipeline` `spawn_blocking` decode + `LossReport` recovery, fixture updates, and tests pass.
- **STEP 8F (`TASK-0128`):** Met. `MIN_VIEWER_WIDTH`, `MIN_VIEWER_HEIGHT`, `ViewerWindowConfig`, `ViewportRect`, `compute_letterbox_viewport`, `format_window_title`, `run_viewer_window`, exports, and pure tests pass.
- **STEP 8G (`TASK-0129`, restaged x2):** Met after restaging (`6086669` / `940cc49`). `.phased/deferred/8G.md` was superseded by the restaged run; `BUG-0028` and `TASK-0130` (filed during the first 8G attempt) are resolved by `6086669`.
- **User Step 8H:** Pending manual execution on target Linux/COSMIC host.
- **Amendments:** None.

### Origin assignment for Independent pass findings
| # | Finding | Class | Origin | Filed |
| :-: | :--- | :--- | :--- | :--- |
| 1 | Uncommitted `Cargo.toml`/`Cargo.lock` leaves `HEAD` unbuildable | `regression` | `plan` / process (`[USER] 8PRE` uncommitted) | `TASK-0131` (high) |
| 2 | `GstreamerAv1Encoder` lacks runtime `vaav1enc` -> `av1enc` fallback and error teardown | `new defect` | `plan` + `coder` | `BUG-0029` (high) |
| 3 | `GstreamerAv1Decoder` permanently disables `vaav1dec` and stalls 4 s on any single corrupt/orphaned frame | `new defect` | `plan` + `coder` (STEP 8G item 5 did not distinguish bad input from HW failure) | `BUG-0030` (medium) |
| 4 | `GstreamerAv1Encoder` jumps PTS backwards when `update_framerate` increases FPS | `new defect` | `coder` | `BUG-0031` (medium) |
| 5 | Host capture loop runs blocking `encoder.encode()` on Tokio async worker thread | `new defect` | `plan` + `coder` | `BUG-0032` (medium) |
| 6 | `perigee connect --window` silently ignores `--dump-frame` and omits `block_in_place` | `new defect` | `plan` + `coder` | `BUG-0033` (low) |
| 7 | `MediaMode::StandIn` + `HostCodec::Av1` silently clamps to 128x128 and disables HW accel | `new defect` | `plan` (explicit in STEP 8D & restaged STEP 8G item 4) | — (documented by plan) |
| 8 | `GpuState::render` skips `request_redraw()` after `Lost`/`Outdated` surface recovery | `new defect` | `coder` | `BUG-0034` (low) |

### Items ready to close (pending user approval)
- `BUG-0025` (STEP 8A), `BUG-0026` (STEP 8B), `BUG-0027` (STEP 8B), `TASK-0122` (STEP 8B), `BUG-0028` (STEP 8G), `TASK-0130` (STEP 8G).
- Keep open: `TASK-0103` (awaits live follow-seat verification in User Step 8H).

## Handoff

**Verdict:** Conditional pass — all 7 automated steps (8A–8G) met their contracts and pass tests in the working tree, but `HEAD` (`940cc49b`) does not compile cleanly because `[USER] 8PRE` (`Cargo.toml`/`Cargo.lock`) was never committed (`TASK-0131`), and `GstreamerAv1Encoder` lacks runtime VA-API fallback (`BUG-0029`) before live User Step 8H.

**High priority:**
- `TASK-0131`: Commit unstaged `Cargo.toml`, `Cargo.lock`, `crates/perigee-host/Cargo.toml`, `crates/perigee-client/Cargo.toml` so `HEAD` builds cleanly.
- `BUG-0029`: Add runtime `vaav1enc` -> `av1enc` fallback and pipeline teardown on error in `crates/perigee-host/src/encoder_gst.rs`.

**Medium priority:**
- `BUG-0030`: Stop permanently disabling `vaav1dec` / double-stalling 4 s on a single corrupt or orphaned AV1 frame (`crates/perigee-client/src/decode.rs`).
- `BUG-0031`: Accumulate monotonic PTS in `GstreamerAv1Encoder` so `update_framerate` never jumps PTS backwards (`crates/perigee-host/src/encoder_gst.rs`).
- `BUG-0032`: Move blocking `encoder.encode()` off the Tokio async worker thread (`crates/perigee-host/src/daemon.rs`).

**Low priority:**
- `BUG-0033`: Handle or reject `--dump-frame` with `--window`, and wrap `run_viewer_window` in `block_in_place` (`crates/perigee-cli/src/main.rs`).
- `BUG-0034`: Call `request_redraw()` after `Lost`/`Outdated` surface recovery (`crates/perigee-client/src/window.rs`).

**Deferred steps to re-split:** None (STEP 8G was deferred twice, restaged, and completed at `6086669`).
**User decisions:**
1. Commit `[USER] 8PRE` Cargo manifests/lockfile (`TASK-0131`) before running User Step 8H.
2. Confirm closing `BUG-0025`, `BUG-0026`, `BUG-0027`, `TASK-0122`, `BUG-0028`, `TASK-0130` (keep `TASK-0103` open until User Step 8H verifies live follow-seat AV1).
