## Problem Description
Imported from roadmap step `STEP 2H`: Host loop + signals. POST_GATE exit 0 incl. smoke test. Diff matches MUTATION_SPEC 1–6: biased select on shutdown vs accept; shutdown_all_sessions → endpoint close → 2 s wait_idle; SIGINT/SIGTERM; default 127.0.0.1:4433; RFERRAM_BIND_INTERFACE; both service files drop 0.0.0.0; both tests assert what the spec asks
- Step ID: `STEP 2H`
- Command: `/boost`
- Roadmap order: completed
- Target files:
  - `crates/rferram-server/src/{host.rs,lib.rs,main.rs}`
  - `packaging/linux/rferram-server.service`
  - `packaging/macos/com.theinfomoose.rferram-server.plist`

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
