## Problem Description
`crates/perigee-host/src/main.rs:30` — `HostOptions::from_env()?` runs before argument parsing, so a bad `PERIGEE_BIND_ADDR` makes `perigee-host --help` fail too — parse args first, apply env only for `Run` (or let `--bind` override a bad env value). Found in 3C validation.
Migrated from the plan file's Parked list (2026-11-18).

## Reproduction Steps
1. `PERIGEE_BIND_ADDR=not-an-addr perigee-host --help`
2. Observe: error instead of usage text.

## Fix Log & Agent Notes
