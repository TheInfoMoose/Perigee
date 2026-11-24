## Problem Description
Imported from roadmap step `STEP 9D`: Multi-monitor /dev/uinput pointer mapping: EV_REL relative motion and virtual-desktop output coordinate mapping
- Step ID: `STEP 9D`
- Command: `/goal`
- Roadmap order: 4
- Target files:
  - `host backends/uinput.rs`
  - `backends/mod.rs`
  - `daemon.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
