## Problem Description
worker.rs wait_for_worker_exit logs a warning and drops Child on its 2 s timeout or a try_wait error. Child::drop neither kills nor waits, so the worker keeps running as the seat user (holding its Wayland capture) and becomes a zombie once it exits. At launch stop_worker did kill() then wait(). Phase 6 review F1, a regression the 6A contract asked for.
Found during: Phase 6 review.

## Reproduction Steps
Start a follow-seat session. SIGSTOP the capture-worker pid, then stop the session or switch seat users. After 2 s the warning "Capture worker did not exit after its stdin closed" appears and the worker is still present in ps. Fix: on timeout, child.kill() then child.wait().

## Fix Log & Agent Notes
