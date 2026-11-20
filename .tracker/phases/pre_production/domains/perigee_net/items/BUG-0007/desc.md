## Problem Description
P4-07. `crates/perigee-net/src/trust.rs:171-185` uses a fixed <path>.tmp (remove_file can unlink another writer's in-flight file) and renames while the handle is open (sharing-violation risk on Windows; unverified).
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (race; read the code)

## Fix Log & Agent Notes
