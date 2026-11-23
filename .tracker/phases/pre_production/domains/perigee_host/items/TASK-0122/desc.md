## Problem Description
In crates/perigee-host/src/daemon.rs:1599-1660, test_video_backlog_is_bounded_and_drops_excess_frames only tests HostSessionEngine::run_capture_loop_with_video directly. Add unit/integration coverage for force_keyframe preservation across TrySendError::Full drops and PerigeeHostDaemon's dual-channel outgoing_rx + video_rx pump.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
