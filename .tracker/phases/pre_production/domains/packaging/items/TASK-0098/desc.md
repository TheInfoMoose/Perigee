## Problem Description
STEP 5A spec gap: MUTATION_SPEC requires running `cargo update --offline -p rustix@1.1.5 --precise 1.1.6` with "Make no hand edits to Cargo.lock", and POST_GATE asserts `! git diff -U0 -- Cargo.lock | grep -E '^[-+][^-+]' | grep -vqE 'rustix|^[-+]checksum = |^[-+]version = "1\.1\.[56]"$'`. Under `cargo 1.99.1` with workspace `resolver = "3"`, that exact command updates `rustix 1.1.5 -> 1.1.6` (no packages added/removed) AND re-resolves 6 existing `windows-sys` dependency edges in `Cargo.lock` (`arboard`, `errno`, `quinn-udp`, `rustix`, `winapi-util`: `windows-sys 0.61.2 -> 0.52.0`; `nu-ansi-term`: `windows-sys 0.61.2 -> 0.59.0`). Location: `Cargo.lock:178,1128,2014,2992,3321,4622`.
Lapse: L1 marker grep
Kind: scope-lapse
Found during: STEP 5A (Neon)

## Reproduction Steps
n/a (spec gap)

## Fix Log & Agent Notes
