## Problem Description
Imported from roadmap step `STEP 6A`: Capture worker stops through a stdin lifeline; sticky stop, no signals (BUG-0013)
- Step ID: `STEP 6A`
- Command: `/boost`
- Roadmap order: 1
- Target files:
  - `host backends/worker.rs`
  - `host.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
