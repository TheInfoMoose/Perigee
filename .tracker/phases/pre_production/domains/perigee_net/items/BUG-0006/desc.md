## Problem Description
P4-06. `crates/perigee-net/src/identity.rs:82-93` writes straight to identity.key (create_new, 0600), so a crash mid-write leaves a truncated key that fails every later start; `:70` reads the PKCS#8 DER into a Vec that is never zeroized.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (crash timing; read the code)

## Fix Log & Agent Notes
