## Problem Description
Imported from roadmap step `STEP 10BA`: Set FD_CLOEXEC on systemd-activated broker listener FD 3 and add unit tests for FD adoption and MAX_BROKER_CONCURRENT_WORKERS saturation (BUG-0045, TASK-0151)
- Step ID: `STEP 10BA`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host backends/broker.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
