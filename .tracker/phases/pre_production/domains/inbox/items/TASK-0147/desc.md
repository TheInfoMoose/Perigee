## Problem Description
Imported from roadmap step `STEP 10A`: Separate UinputInjector encoded frame dimensions from multi-monitor compositor geometry and eliminate set_output_layout double-lock/race (BUG-0042, BUG-0043)
- Step ID: `STEP 10A`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host backends/uinput.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
