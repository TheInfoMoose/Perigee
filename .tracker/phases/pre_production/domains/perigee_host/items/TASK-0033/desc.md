## Problem Description
Imported from roadmap step `STEP 3A`: Host hardening. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–5: saturating bitrate_kbps_from_bytes_per_sec, LossReport logs len/first/last, AuthResponse send failure calls terminate_session before returning, shutdown_signal falls back to Ctrl-C instead of expect, two new tests
- Step ID: `STEP 3A`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/perigee-host/src/{daemon,host}.rs`

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
