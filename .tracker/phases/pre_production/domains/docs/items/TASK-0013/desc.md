## Problem Description
Imported from roadmap step `STEP 1G`: Media backends report. POST_GATE exit 0. Spot-checked all 4 constant-fill capture sites, X11 .max(fallback) upscaling, no xtest_fake_input call, hardcoded 2560x1440 in lib.rs:107-118, 10 unsafe sites. Claude ran cargo test -p rferram-server --all-targets --locked: 21 passed
- Step ID: `STEP 1G`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `docs/audit/07-media-backends.md`

## Reproduction Steps
1. Inspect `docs`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `docs`
