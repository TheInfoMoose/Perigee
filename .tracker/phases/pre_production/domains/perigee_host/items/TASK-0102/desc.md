## Problem Description
`crates/perigee-host/src/host.rs` logs `Failed to serve incoming QUIC connection error=%e` and `crates/perigee-host/src/daemon.rs` sends `e.to_string()` in the AuthResponse, so only the outermost anyhow context reaches the log and client ("failed to create the /dev/uinput devices"); the OS error (EACCES) was lost and had to be found with getfacl. Suggested fix: format with `{e:#}` in both places.
Found during: User Step 5R (Phase 5a).

## Reproduction Steps
Connect to a --media follow-seat host whose service user cannot open /dev/uinput.

## Fix Log & Agent Notes
