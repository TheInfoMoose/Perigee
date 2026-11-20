## Problem Description
P4-10 / CLI-10. `crates/perigee-client/src/session.rs:385` does info!(?vc, ...) on every WireMessage::Channel, writing clipboard text/image bytes to the log. Log variant and length at debug instead (host side was fixed the same way for LossReport in 3A).
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
Run a client session with RUST_LOG=info and send a clipboard message from the host; the payload appears in the log.

## Fix Log & Agent Notes
