## Problem Description
`crates/perigee-client/src/session.rs:379` — `warn!(?missing_frames, ...)` logs the whole peer-sized list; 3A fixed the same issue on the host — log len/first/last. Found in 3E validation.
Migrated from the plan file's Parked list (2026-11-18).

## Reproduction Steps
n/a (parked finding; see Problem Description)

## Fix Log & Agent Notes
