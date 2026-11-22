## Problem Description
`crates/perigee-host/src/daemon.rs` `test_stop_ends_capture_loop_blocked_in_capture` (STEP 5BG) discards its 2 s timeout with `.ok()`, then asserts `!is_running()`, which `stop()` sets synchronously. The test passes even if the capture wait never races the stop signal. Suggested fix: `.expect(...)` the timeout, and always release the fake capturer.
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
By reading: remove the `stop_rx.changed()` arm from the capture select; the timeout elapses, is discarded, and the assert still holds.

## Fix Log & Agent Notes
