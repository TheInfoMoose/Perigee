## Problem Description
crates/perigee-host/src/daemon.rs:378: encoder.lock().await.encode(&captured_frame, need_keyframe) runs synchronously on a Tokio async worker thread (blocking up to 2-4s on GStreamer try_pull_sample or pipeline rebuilds and delaying stop_rx handling), unlike capture_next_frame above it which uses spawn_blocking and races against stop_rx.changed().

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
