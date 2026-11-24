## Problem Description
`crates/perigee-host/src/daemon.rs:137-157` and `crates/perigee-host/src/backends/linux_capture.rs:278-280` — When `HostSessionEngine::with_media_and_codec` runs with `MediaMode::StandIn` and `HostCodec::Av1` (using `EncoderConfig::default()` at 2560x1440), `WaylandDmabufCapturer::capture_next_frame` and `X11ShmCapturer::capture_next_frame` cap `CapturedFrame.data` at `.min(64 * 1024)` (65,536 bytes), whereas `GstreamerAv1Encoder::validate_frame` requires `stride * (height - 1) + width * 4` (14,745,600 bytes). Every frame fails encoding (`daemon.rs:406`), so no AV1 frames are ever sent — suggested fix: for `(MediaMode::StandIn, HostCodec::Av1)` in `HostSessionEngine::with_media_and_codec`, clamp/set stand-in capture dimensions so `width * height * 4 <= 64 * 1024` (e.g., `128x128`, which is exactly 65,536 bytes and encodes quickly in software `av1enc`).
Kind: bug
Found during: STEP 8G (Neon)

## Reproduction Steps
1. `cargo test -p perigee-cli --test loopback test_real_host_and_client_av1_roundtrip --locked`
2. Observe: `timed out waiting for decoded AV1 frame #0` (host logs `Video frame encoding failed` on every frame because `data.len() == 65536 < 14745600`).

## Fix Log & Agent Notes
