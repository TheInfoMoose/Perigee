## Problem Description
Imported from roadmap step `STEP 3D`: Client connect API. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–4: connect.rs (constants, resolve_host_addr identical to the UI resolver, parse_interface_binding, ConnectOptions, ClientSession, connect waits for Active/Terminated/timeout and stops + closes on every failure path); session.rs Connecting→Authenticating via send_if_modified, rejected AuthResponse → Terminated, NegotiatingTopology removed; five new tests; existing pipeline test waits for Authenticating instead of asserting Connecting
- Step ID: `STEP 3D`
- Command: `/boost`
- Roadmap order: completed
- Target files:
  - `crates/perigee-client/src/{connect.rs (NEW),lib,session}.rs`

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
