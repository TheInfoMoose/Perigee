## Problem Description
`crates/perigee-host/src/daemon.rs` `build_live_media` has no CaptureStop, and `backends/wayland_shm.rs` `capture_once` waits in `blocking_dispatch` until the compositor delivers. After a stop the blocking thread stays until the screen changes. Dropping the tokio runtime waits for blocking tasks without a limit, so since STEP 5BG host shutdown can hang the same way. Suggested fix: a cancellable Wayland wait (poll the queue fd plus a wake fd).
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
Expected from the code, not run: `perigee host --media live`, connect, leave the screen static, then Ctrl-C the host.

## Fix Log & Agent Notes
