## Problem Description
`crates/perigee-host/src/host.rs:73` — `pub fn from_env()` has no callers after STEP 4B/4C (both binaries now call `resolve_*_command(&args, |k| std::env::var(k).ok())`, which uses `from_env_with`) — suggested fix: delete it, or keep it only if an embedding caller needs it.
Found during: STEP 4C validation.

## Reproduction Steps
n/a (planned work). `grep -rn 'from_env()' crates --include=*.rs` shows only the definition.

## Fix Log & Agent Notes
