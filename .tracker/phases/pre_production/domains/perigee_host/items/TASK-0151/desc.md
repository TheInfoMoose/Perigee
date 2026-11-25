## Problem Description
crates/perigee-host/src/backends/broker.rs:662-893 lacks unit tests for MAX_BROKER_CONCURRENT_WORKERS saturation ('too many active workers') and bind_broker_listener's LISTEN_FDS=1 / LISTEN_PID socket-activation adoption path (including FD_CLOEXEC verification).

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
