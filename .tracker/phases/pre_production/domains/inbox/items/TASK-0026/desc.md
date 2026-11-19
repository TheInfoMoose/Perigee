## Problem Description
Imported from roadmap step `STEP 2G`: Session reaper. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–4: reaper selects on wait_for(stopped) / connection.closed(), removes from registry, closes with "session ended"; datagram + bi loops stop only when close_reason() is set, otherwise log at debug and continue; loopback test now polls for removal; client-close test added. Parked 2F leak (daemon.rs AuthResponse send) is still open: reaper is spawned after that send
- Step ID: `STEP 2G`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/rferram-server/src/daemon.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
