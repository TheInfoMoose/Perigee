## Problem Description
Imported from roadmap step `STEP 8BA`: Runtime vaav1enc -> av1enc fallback, error teardown, and monotonic PTS across update_framerate (BUG-0029, BUG-0031)
- Step ID: `STEP 8BA`
- Command: `/goal`
- Roadmap order: 1
- Target files:
  - `host encoder_gst.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
