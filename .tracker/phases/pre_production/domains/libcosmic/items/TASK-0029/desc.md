## Problem Description
Imported from roadmap step `STEP 2J`: Reserved placeholders. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–5: distinct 192.0.2.x per literal, ports kept; /tmp/rferram-test/... paths; testuser fixtures; runner.rs edit sequence recomputed (caret 10, backspace/delete results correct); app.rs caret string is still 15 chars so caret assertions hold
- Step ID: `STEP 2J`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `8 crate files`
  - `vendor/libcosmic/src/{raster,runner}.rs`

## Reproduction Steps
1. Inspect `vendor/libcosmic`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `vendor/libcosmic`
