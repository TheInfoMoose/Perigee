## Problem Description
Imported from roadmap step `STEP 2K`: Rust rename to Perigee. POST_GATE exit 0 incl. PERIGEE_SMOKE_TEST. 50 paths (45 renames + 5 modified). Normalized diff (rferram/perigee folded, server→host) leaves only rustfmt import reorders and one description cleanup: no logic change. Lockfile: exactly the 7 member names swapped. Test attributes 120 → 120; 101 passed, 0 failed. repository URL and ST patch table unchanged
- Step ID: `STEP 2K`
- Command: `/boost`
- Roadmap order: completed
- Target files:
  - `Cargo.toml`
  - `Cargo.lock`
  - `crates/`
  - `vendor/libcosmic/src/{layout,raster,runner}.rs`

## Reproduction Steps
1. Inspect `vendor/libcosmic`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `vendor/libcosmic`
