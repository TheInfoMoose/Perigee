## Problem Description
crates/perigee-host/src/encoder_gst.rs:286-293, 419-430: encode_once computes pts = (frame_idx + 1) * duration_ns using current target_fps. Increasing target_fps mid-stream via update_framerate shrinks duration_ns and causes PTS to jump backwards, violating GStreamer monotonic PTS expectations. Accumulate next_pts_ns monotonically instead.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
