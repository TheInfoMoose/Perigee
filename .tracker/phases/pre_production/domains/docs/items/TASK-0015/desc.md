## Problem Description
Imported from roadmap step `STEP 1I`: UI coupling report. POST_GATE exit 0. Spot-checked UI-04 (vendor/libcosmic/Cargo.toml deps are taffy/tiny-skia/winit/wgpu, no iced; 8,555 LOC; all 5 commits in-repo), UI-02 (init never calls with_persistence_config; only tests do), rferram-* class constants in layout.rs:18
- Step ID: `STEP 1I`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `docs/audit/09-ui-coupling.md`

## Reproduction Steps
1. Inspect `docs`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `docs`
