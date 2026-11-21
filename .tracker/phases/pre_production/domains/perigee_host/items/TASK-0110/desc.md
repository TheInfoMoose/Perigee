## Problem Description
Imported from roadmap step `STEP 5BG`: Capture on a blocking thread, stop-aware sends, worker stop, frame timing (BUG-0013, TASK-0099, TASK-0103)
- Step ID: `STEP 5BG`
- Command: `/boost`
- Roadmap order: 7
- Target files:
  - `host daemon.rs`

Advances: BUG-0013, TASK-0099, TASK-0103 (timing)

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
