## Problem Description
Imported from roadmap step `STEP 6B`: A failed seat poll repeats the last frame instead of ending the session (BUG-0018)
- Step ID: `STEP 6B`
- Command: `/goal`
- Roadmap order: 2
- Target files:
  - `host backends/worker.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
