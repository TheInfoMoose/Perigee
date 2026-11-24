## Problem Description
Imported from roadmap step `STEP 8BB`: Preserve vaav1dec and avoid double-stalling on corrupt or orphaned AV1 frames (BUG-0030)
- Step ID: `STEP 8BB`
- Command: `/goal`
- Roadmap order: 2
- Target files:
  - `client decode.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
