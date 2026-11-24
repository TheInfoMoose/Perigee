## Problem Description
STEP 8G spec gap: `test_real_host_and_client_av1_roundtrip` requires binding `perigee-host` with `HostOptions { media: MediaMode::StandIn, codec: HostCodec::Av1, .. }` and receiving 2 decoded AV1 `Rgba8` frames vs `crates/perigee-host/src/daemon.rs:137-157` uses `EncoderConfig::default()` (2560x1440) with a stand-in capturer capped at 64 KiB (`crates/perigee-host/src/backends/linux_capture.rs:278-280`), causing `GstreamerAv1Encoder::validate_frame` to reject every frame. Fixing this requires editing `crates/perigee-host/src/daemon.rs`, which is outside STEP 8G's `ALLOWED_FILES`. Location: `crates/perigee-host/src/daemon.rs:137-157`.
Lapse: L2 file outside ALLOWED_FILES
Kind: scope-lapse
Found during: STEP 8G (Neon)

## Reproduction Steps
n/a (spec gap)

## Fix Log & Agent Notes
