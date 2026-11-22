## Problem Description
Imported from roadmap step `STEP 6C`: Cancellable Wayland wait; --media live capture ends on session stop (BUG-0016)
- Step ID: `STEP 6C`
- Command: `/boost`
- Roadmap order: 3
- Target files:
  - `host backends/wayland_shm.rs`
  - `backends/mod.rs`
  - `daemon.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
