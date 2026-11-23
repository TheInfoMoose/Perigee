## Problem Description
Imported from roadmap step `STEP 7A`: Kill and reap hung capture workers after timeout; tighten reap test budget (BUG-0021, BUG-0022)
- Step ID: `STEP 7A`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host backends/worker.rs`
  - `packaging/linux/perigee-host.service`

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
