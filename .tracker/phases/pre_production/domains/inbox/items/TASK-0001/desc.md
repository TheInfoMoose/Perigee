## Problem Description
`Cargo.toml:135-138` (`[patch."https://github.com/TheInfoMoose/ST_Rust_Age"]`) and `:36,38` — since 2B, `simply-transfer-core` / `simply-transfer-snapshots` aren't in the crate graph, so cargo warns "patch ... was not used" every run — drop both patch entries and their `[workspace.dependencies]` lines when the ST link crate is adopted (Phase 4), or sooner. Cargo.toml change; needs user OK. Scope: workspace root (no workspace scope exists).
Migrated from the plan file's Parked list (2026-11-18).

## Reproduction Steps
n/a (parked finding; see Problem Description)

## Fix Log & Agent Notes
