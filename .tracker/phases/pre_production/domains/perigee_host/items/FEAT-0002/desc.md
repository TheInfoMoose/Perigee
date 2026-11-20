## Problem Description
P4-02 / AUTH-05. `crates/perigee-host/src/daemon.rs:417-420` awaits recv_uni_message with no timeout; `host.rs:381-413` spawns per accepted connection with no cap or rate limit. Untrusted peers fail TLS, but handshake attempts are unlimited.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (planned work)

## Fix Log & Agent Notes
