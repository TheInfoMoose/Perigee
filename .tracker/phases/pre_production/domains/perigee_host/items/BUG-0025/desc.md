## Problem Description
In crates/perigee-host/src/backends/worker.rs:192-193 and 206-207, wait_for_worker_exit discards the Result from child.kill() and unconditionally calls blocking child.wait(). If child.kill() fails with EPERM (e.g. CAP_KILL absent after UID switch), child.wait() hangs indefinitely instead of logging the error and returning after WORKER_EXIT_TIMEOUT.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
