## Problem Description
The test allows 3 s, but WORKER_EXIT_TIMEOUT is 2 s. If stop_worker keeps the stdout reader while waiting (the 5R deadlock), the wait times out at 2 s, capture_next_frame still returns the repeated frame, and the test passes with the worker leaked. Phase 6 review F2.
Found during: Phase 6 review.

## Reproduction Steps
Move drop(reader) after wait_for_worker_exit in stop_worker, then run cargo test -p perigee-host --locked test_worker_error_reaps_worker_blocked_on_write. It passes. Fix: use a budget below WORKER_EXIT_TIMEOUT, or assert the child has exited or been reaped.

## Fix Log & Agent Notes
