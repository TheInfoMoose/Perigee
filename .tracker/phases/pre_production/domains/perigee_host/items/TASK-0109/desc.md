## Problem Description
Imported from roadmap step `STEP 5BF`: WorkerStopper kills a blocked capture worker (BUG-0013)
- Step ID: `STEP 5BF`
- Command: `/goal`
- Roadmap order: 6
- Target files:
  - `host backends/worker.rs`
  - `backends/mod.rs`

Advances: BUG-0013

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
