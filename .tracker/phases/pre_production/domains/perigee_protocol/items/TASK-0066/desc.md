## Problem Description
P4-09. `crates/perigee-protocol/src/lib.rs:155` AuthResponse.requires_elevation is always false (elevation removed in 2B); `crates/perigee-host/src/daemon.rs:448` fallback says "none (legacy transport)" though only mTLS exists; `crates/perigee-common/src/models.rs:64` HostCredentials is test-only. Related: TASK-0039.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (planned work)

## Fix Log & Agent Notes
