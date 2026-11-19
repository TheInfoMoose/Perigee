## Problem Description
Imported from roadmap step `STEP 3C`: Host options + CLI args. POST_GATE exit 0 (incl. smoke run with --bind 127.0.0.1:0 and --help). Diff matches MUTATION_SPEC 1–4: constants, HostOptions (from_env_with/from_env/transport_config), HostCommand, parse_host_args, HOST_USAGE, bind_host; main.rs delegates; three tests. One non-test .expect in HostOptions::default() parses a constant (infallible)
- Step ID: `STEP 3C`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/perigee-host/src/{host,lib,main}.rs`

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
