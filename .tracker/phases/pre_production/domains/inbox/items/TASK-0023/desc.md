## Problem Description
Imported from roadmap step `STEP 2D`: Fail-closed socket bind. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–6: reuse flags gone; socket domain taken from the address actually bound; both InterfaceName paths return refusing to fall back instead of binding bind_addr; lo/lo0 test gated per OS; two new tests. macOS/Windows paths compile-checked by CI only
- Step ID: `STEP 2D`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/rferram-net/src/socket.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
