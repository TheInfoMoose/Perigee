## Problem Description
`crates/perigee-host/src/daemon.rs` run_capture_loop calls `capture_next_frame()` inside `tokio::spawn` while holding the capturer tokio Mutex. WaylandShmCapturer blocks in `blocking_dispatch` and WorkerCapturer blocks reading the worker pipe, so each live session pins a runtime worker thread. Fine at 10 fps with one session; it starves other tasks as sessions grow. Suggested fix: run capture in `spawn_blocking` or a dedicated thread feeding an mpsc channel.
Found during: Phase 5a review (5H/5N).

## Reproduction Steps
n/a (design review)

## Fix Log & Agent Notes
