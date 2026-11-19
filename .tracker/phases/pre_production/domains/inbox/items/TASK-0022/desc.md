## Problem Description
Imported from roadmap step `STEP 2C`: Unused deps pruned. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–4 exactly; no remaining quinn_proto/libc::/windows_sys uses; rferram-common only used by net/client tests. Lockfile: fastbloom, siphasher removed (quinn-proto feature set), 0 added, 0 version changes. windows-sys removal is compile-checked only by CI
- Step ID: `STEP 2C`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/rferram-{common,net,client,server}/Cargo.toml`
  - `Cargo.lock`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
