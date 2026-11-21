## Problem Description
Imported from roadmap step `STEP 5F`: UinputInjector (/dev/uinput keyboard + absolute pointer)
- Step ID: `STEP 5F`
- Command: `/boost`
- Roadmap order: 6
- Target files:
  - `host backends/uinput.rs`
  - `backends/mod.rs`
  - `Cargo.toml`
  - `Cargo.lock`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
