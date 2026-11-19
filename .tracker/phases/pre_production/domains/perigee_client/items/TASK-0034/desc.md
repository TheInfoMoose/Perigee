## Problem Description
Imported from roadmap step `STEP 3B`: Client loss-report bounds. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–3: MAX_LOSS_REPORT_FRAMES = 64 with take(), warn logs gap/reported counts, late frames log at debug only, expected ID uses max(.., saturating_add(1)); three new tests (1→1,000,000 gives 64 IDs from 2; 1,3,2,4 gives one report [2]; u64::MAX then 5 gives no report)
- Step ID: `STEP 3B`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/perigee-client/src/session.rs`

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
