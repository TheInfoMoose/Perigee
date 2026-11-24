## Problem Description
crates/perigee-client/src/window.rs:435-438: On CurrentSurfaceTexture::Lost or Outdated, GpuState::render reconfigures the swapchain and returns Ok(()) without calling self.window.request_redraw(), leaving a static screen stale until the next incoming frame or OS redraw.

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
