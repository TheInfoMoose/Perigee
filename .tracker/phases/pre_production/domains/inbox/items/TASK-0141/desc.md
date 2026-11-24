## Problem Description
Imported from roadmap step `STEP 9E`: Linux DMA-BUF capture (ExtDmabufCapturer via ext-image-copy-capture-v1 + zwp_linux_dmabuf_v1 + gbm) with SHM fallback and --capture-mode
- Step ID: `STEP 9E`
- Command: `/boost`
- Roadmap order: 5
- Target files:
  - `host backends/wayland_dmabuf.rs`
  - `backends/mod.rs`
  - `host.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
