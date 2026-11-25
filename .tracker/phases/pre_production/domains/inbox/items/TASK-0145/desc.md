## Problem Description
Imported from roadmap step `STEP 9BB`: Forward Wayland DisplayLayout from capture-worker through WorkerCapturer into UinputInjector::set_output_layout (BUG-0041)
- Step ID: `STEP 9BB`
- Command: `/goal`
- Roadmap order: 2
- Target files:
  - `host host.rs`
  - `backends/worker.rs`
  - `backends/mod.rs`
  - `daemon.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
