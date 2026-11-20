## Problem Description
`crates/perigee-cli/Cargo.toml:22` — `perigee-net` is declared but neither `src/` nor `tests/loopback.rs` uses `perigee_net` (STEP 3G spec listed it; `perigee-protocol` is used by the loopback test) — remove the line and run `cargo update --workspace --offline` (dependency-line removal only).
Found during: STEP 3H validation.

## Reproduction Steps
n/a (planned work)

## Fix Log & Agent Notes
