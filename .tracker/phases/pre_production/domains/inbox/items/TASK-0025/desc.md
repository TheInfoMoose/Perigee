## Problem Description
Imported from roadmap step `STEP 2F`: Capture errors stop the session. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–6; fallback capturer and synthetic frame deleted; FailingCapturer test proves stop. Gemini moved provision_session ahead of the success AuthResponse (needed so item 5's failure reply isn't sent after a success) — accepted
- Step ID: `STEP 2F`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/rferram-server/src/daemon.rs`
  - `crates/rferram-server/src/lib.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
