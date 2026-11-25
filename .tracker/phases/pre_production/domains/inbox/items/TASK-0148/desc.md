## Problem Description
Imported from roadmap step `STEP 10B`: Implement Linux capture-broker module: Unix socket protocol, SCM_RIGHTS FD passing, non-root session validation, and broker-owned worker lifecycle (TASK-0100)
- Step ID: `STEP 10B`
- Command: `/boost`
- Roadmap order: 2
- Target files:
  - `host backends/broker.rs`
  - `backends/worker.rs`
  - `backends/mod.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
