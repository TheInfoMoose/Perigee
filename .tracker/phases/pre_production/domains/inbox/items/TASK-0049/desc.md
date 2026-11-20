## Problem Description
Imported from roadmap step `STEP 4I`: Host on mTLS: config_dir, --config-dir, TrustFileAuthorizer; loopback test trusts its client
- Step ID: `STEP 4I`
- Command: `/boost`
- Roadmap order: 9
- Target files:
  - `crates/perigee-host/src/{host,lib}.rs`
  - `crates/perigee-cli/tests/loopback.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
