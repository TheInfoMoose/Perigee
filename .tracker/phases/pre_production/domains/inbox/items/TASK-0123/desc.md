## Problem Description
Imported from roadmap step `STEP 8A`: Guard child.wait() in wait_for_worker_exit when child.kill() fails (BUG-0025)
- Step ID: `STEP 8A`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host backends/worker.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
