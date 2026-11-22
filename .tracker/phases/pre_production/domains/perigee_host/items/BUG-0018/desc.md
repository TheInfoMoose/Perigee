## Problem Description
`crates/perigee-host/src/backends/worker.rs` `capture_next_frame` propagates any `poll_seat` error, and the capture loop then stops the session. A logind session or user vanishing between the three loginctl calls, or one failed spawn, ends the remote session during the login/logout switch it should survive. Suggested fix: warn, repeat the last frame, retry at the next poll.
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
By reading; not seen in the 5R journal. Use a fake loginctl that fails once: capture_next_frame returns Err.

## Fix Log & Agent Notes
