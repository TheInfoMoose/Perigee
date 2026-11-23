## Problem Description
Imported from roadmap step `STEP 8B`: Re-arm force_keyframe on dropped keyframes/inter-frame video, decouple outgoing_rx from video pump, add tests (BUG-0026, BUG-0027, TASK-0122)
- Step ID: `STEP 8B`
- Command: `/goal`
- Roadmap order: 2
- Target files:
  - `host daemon.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
