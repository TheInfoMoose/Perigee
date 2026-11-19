## Problem Description
`crates/perigee-client/src/bridge.rs:234` — workaround: `disconnect` sleeps 20 ms after sending `Disconnect` because `send_uni_message` only calls `finish()` and the immediate `close()` can discard unsent stream data; on a real-latency link the host may never see `Disconnect` (the host reaper still ends the session on `closed()`) — await delivery (`SendStream::stopped()` or a host ack). Found in 3E validation.
Migrated from the plan file's Parked list (2026-11-18).

## Reproduction Steps
n/a (parked finding; see Problem Description)

## Fix Log & Agent Notes
