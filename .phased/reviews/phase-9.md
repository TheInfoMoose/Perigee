# Phase 9 review

Range: `7819d29a7396161f63ee41a7e52e5ca93efc4005..ec7d673454992c81ac03b36c07829566f81417ac`

## Independent pass

### Findings

1. **`crates/perigee-host/src/backends/uinput.rs:139-221` & `crates/perigee-host/src/backends/wayland_shm.rs:233-260` — Mixed logical/physical coordinate spaces in multi-monitor `EV_ABS` pointer mapping (`new defect`)**
   - **Trigger:** Multi-monitor Wayland layout where at least one output has `scale_factor != 1.0` and `zxdg_output_v1` reports logical `x`/`y` coordinates (`logical_x`, `logical_y`).
   - **Effect:** `output_info_to_monitor_info` populates `MonitorInfo::x` and `y` with logical coordinates while `width` and `height` remain physical pixel dimensions (`wl_output::Mode`). `compute_virtual_desktop_bounds` and `map_output_pointer_to_abs` then add physical `width`/`height` to logical `x`/`y`, inflating the virtual desktop bounding box and mapping `EV_ABS` coordinates to the wrong position or monitor (e.g., clicking the center of a 2x-scaled `3840x2160` primary output at `(0, 0)` computes `global_x = 1920`, landing on an adjacent output starting at logical `x = 1920`).
   - **Sibling:** `UinputInjector::set_screen_size` (called by `screen_sink` in `crates/perigee-host/src/daemon.rs:262-264`) updates `screen_width`/`screen_height` atomics on mode change without updating `output_layout.monitors`.

2. **`crates/perigee-host/src/daemon.rs:288-315` — `follow-seat` media never populates `UinputInjector::set_output_layout` (`new defect`)**
   - **Trigger:** Running `perigee-host --media follow-seat` on a multi-monitor host or with `--output <N>` (`N > 0`).
   - **Effect:** Unlike `build_live_media` (`daemon.rs:256-260`), `build_follow_seat_media` never calls `uinput.set_output_layout(...)` (and `WorkerCapturer` does not forward enumerated `MonitorInfo` from `capture-worker`). `UinputInjector::output_layout` stays empty, so absolute pointer events fall back to single-monitor `move_to` across the full compositor desktop instead of targeting output `<N>`.

### What was traced
- `Cargo.lock` & `crates/perigee-host/Cargo.toml`: added `gbm` (`0.18.0`) and transitive `drm`/`drm-ffi`/`drm-sys`/`wayland-server` crates.
- `crates/perigee-client/src/decode.rs`: test-only addition (`encode_synthetic_av1_keyframe` and `test_gstreamer_av1_decoder_decode_fallback_gating`) exercising real `av1enc` -> `GstreamerAv1Decoder::decode` pre/post-decode fallback gating.
- `crates/perigee-host/src/encoder_gst.rs`: test-only update to `test_gstreamer_av1_encoder_software_fallback_and_fault_recovery` exercising `encode()`'s runtime `use_vaapi` fallback branch.
- `crates/perigee-host/src/backends/wayland_dmabuf.rs`: `CaptureMode`, `parse_capture_mode`, `pick_dmabuf_format`, `resolve_render_node_from_dev_t`, `ExtDmabufCapturer`, and `WaylandCapturer` (`Auto` fallback to `WaylandShmCapturer`, `BufferConstraints` reallocation, cancel handling).
- `crates/perigee-host/src/backends/wayland_shm.rs`: `zxdg_output_v1` integration, `wl_output::Mode::Current` filtering, `output_info_to_monitor_info`, `list_wayland_outputs`, `format_monitor_list`.
- `crates/perigee-host/src/backends/uinput.rs`: `REL_X`/`REL_Y` relative pointer injection, `compute_virtual_desktop_bounds`, `map_output_pointer_to_abs`, `map_relative_pointer_delta`, `set_output_layout`.
- `crates/perigee-host/src/backends/worker.rs`: `WorkerCapturer::output_index()`.
- `crates/perigee-host/src/headless.rs`: read-only sysfs/configfs/procfs probe (`probe_headless_paths`, `probe_headless_system`, `format_headless_report`).
- `crates/perigee-host/src/host.rs` & `lib.rs`: `--output` / `PERIGEE_OUTPUT`, `outputs`, `headless-check`, `--capture-mode` on `snapshot` and `capture-worker`.
- Verification run at `HEAD`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p perigee-host --all-targets --locked` (98 passed), and `cargo test -p perigee-client --all-targets --locked` (36 passed).

### What was not checked
- Live Wayland compositor / DRM / `vkms` runtime behavior (`outputs`, `snapshot --capture-mode dmabuf`, `headless-check`, live multi-output `--output`, `uinput` pointer injection against a real compositor).
- Unit tests in workspace crates other than `perigee-host` and `perigee-client`.

<!-- SEALED: Independent pass locked -->

## Contract pass

- **Contract compliance:** All six Phase 9 steps (`9A`–`9F`) and `[USER] 9PRE` (`gbm` in `Cargo.lock`) are implemented and pass `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `perigee-host` (98 tests), and `perigee-client` (36 tests). `docs/headless-vkms-spike.md` is present.
- **Deferrals & amendments:** None in `.phased/deferred/` or `.phased/amendments/` for Phase 9.
- **Finding origins:**
  1. `BUG-0040` (HiDPI mixed logical/physical coordinates in `uinput.rs` / `wayland_shm.rs`) — origin: `plan` + `coder` (`STEP 9B` placed logical `x`/`y` and physical `width`/`height` on `MonitorInfo`; `STEP 9D` used `MonitorInfo::width`/`height` directly in `compute_virtual_desktop_bounds` and `map_output_pointer_to_abs` without `scale_factor` normalization).
  2. `BUG-0041` (`follow-seat` never populates `UinputInjector::set_output_layout`) — origin: `plan` (`STEP 9D` only wired `build_live_media`, omitting `build_follow_seat_media`).
- **Ready to close (`Advances:` from `STEP 9A`):** `BUG-0035` (`perigee_host`, low) and `BUG-0036` (`perigee_client`, low).
- **Note:** `--media live` (`daemon.rs:248`) still uses `WaylandShmCapturer` rather than `WaylandCapturer`, matching `STEP 9E`'s scope (`snapshot` and `capture-worker` only).

## Handoff

- **Verdict:** PASS WITH FINDINGS (2 medium-severity `plan`/`coder` bugs filed as `pending`; all Phase 9 contracts `9A`–`9F` met).
- **New pending items (medium):**
  - `BUG-0040` (`perigee_host`): `uinput` multi-monitor `EV_ABS` mapping mixes logical `x`/`y` with physical `width`/`height` under HiDPI scaling (plus `screen_sink` not updating `output_layout`).
  - `BUG-0041` (`perigee_host`): `follow-seat` media never calls `UinputInjector::set_output_layout`.
- **Ready to close (awaiting user `approved`):** `BUG-0035`, `BUG-0036`.
- **Deferred steps to re-split:** None.
- **User verification pending:** `[USER] 9G` (live COSMIC multi-output `outputs` / `--output`, `--capture-mode dmabuf`, `vkms` headless KMS spike).
- **Decisions for user:** Confirm closing `BUG-0035` and `BUG-0036`; decide whether `BUG-0040`/`BUG-0041` and `--media live` `WaylandCapturer` adoption go into Phase 10.

