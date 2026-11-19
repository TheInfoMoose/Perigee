## Problem Description
Imported from roadmap step `STEP 3E`: Client teardown. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–5: pipeline + bridge store JoinHandles, close watcher sets Terminated("connection closed: …"), disconnect sends Disconnect (1 s bound), stops, closes, sets Terminated, joins under 2 s then aborts; tasks_finished with drained; connect failure paths use disconnect. Beyond spec but in scope: outbound pump also exits on closed(), demux/input tasks exit on Terminated, disconnect_lock serialises concurrent calls. Three new tests
- Step ID: `STEP 3E`
- Command: `/boost`
- Roadmap order: completed
- Target files:
  - `crates/perigee-client/src/{bridge,session,connect}.rs`

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
