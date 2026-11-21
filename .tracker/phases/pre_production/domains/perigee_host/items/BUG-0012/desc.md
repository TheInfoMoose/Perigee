## Problem Description
`crates/perigee-host/src/seat.rs` query_active_session returns None unless the session Type is `wayland`. On Pop!_OS 24.04 COSMIC, greetd starts the greeter (`/etc/greetd/cosmic-greeter.toml`: user cosmic-greeter, command cosmic-greeter-start → `cosmic-comp cosmic-greeter`, vt 1) without XDG_SESSION_TYPE, so logind records it as tty/unspecified (suspected exact value). Result: at the login screen the follow-seat host starts no capture worker and clients get the black 64x64 placeholder (User Step 5R, 2026-11-21 14:34 session 11). Suggested fix: accept Class=greeter regardless of Type (the worker already finds the wayland-N socket and fails cleanly), and add a seat.rs test with Class=greeter Type=tty.
Found during: User Step 5R (Phase 5a).

## Reproduction Steps
Install the package, log out to the greeter, `perigee connect <host> --dump-frame g.ppm --for 5` → 64x64 black frame; journal shows no "Starting capture worker".

## Fix Log & Agent Notes
