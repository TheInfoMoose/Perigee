## Problem Description
Imported from roadmap step `STEP 1H`: Client report. POST_GATE exit 0. Spot-checked CLI-03 (WgpuRenderPipeline holds no wgpu handles; request_device only after #[cfg(test)] at render.rs:784), CLI-08 (unbounded collect() + frame_idx + 1 at session.rs:204,215), CLI-09 (stop() only flips a flag), e2e test builds its own AuthRequest
- Step ID: `STEP 1H`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `docs/audit/08-client.md`

## Reproduction Steps
1. Inspect `docs`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `docs`
