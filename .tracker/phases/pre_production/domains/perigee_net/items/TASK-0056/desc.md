## Problem Description
Imported from roadmap step `STEP 4P`: Delete legacy bind/connect + PeerCertChannelVerifier (NET-01); drop net dev-dep
- Step ID: `STEP 4P`
- Command: `/goal`
- Roadmap order: 16
- Target files:
  - `crates/perigee-net/{src/endpoint.rs,src/manager.rs,tests/quic_tests.rs,Cargo.toml}`
  - `host host.rs tests`
  - `Cargo.lock`

## Reproduction Steps
1. Inspect `crates/perigee-net`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-net`
