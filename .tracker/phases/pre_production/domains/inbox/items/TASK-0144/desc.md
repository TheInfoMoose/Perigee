## Problem Description
Imported from roadmap step `STEP 9BA`: Fix HiDPI logical/physical coordinate mixing in uinput multi-monitor EV_ABS mapping and sync output_layout in set_screen_size (BUG-0040)
- Step ID: `STEP 9BA`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host backends/uinput.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
