## Problem Description
`Cargo.lock` has rustix 1.1.5; `cargo deny --offline check advisories` fails on RUSTSEC-2026-0486 (unsound xattr functions with zero-length buffers; patched in >= 1.1.6), so CI's security-audit job (`.github/workflows/ci.yml` cargo audit --deny warnings / cargo deny) fails. Unreachable here per docs/audit/12-claude-validation.md BASE-05 (no xattr calls). Suggested fix: bump rustix to >= 1.1.6 when online (lockfile change, needs approval), or add a reasoned ignore. Found during: STEP 4BH validation.

## Reproduction Steps
cargo deny --offline check advisories

## Fix Log & Agent Notes
