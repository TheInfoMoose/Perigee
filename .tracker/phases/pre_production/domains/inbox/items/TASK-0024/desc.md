## Problem Description
Imported from roadmap step `STEP 2E`: Accept loop survives bad handshakes. POST_GATE exit 0. Diff matches MUTATION_SPEC 1–4: while let over endpoint.accept(), failures logged at warn with remote address, None only after close; renamed ALPN test now proves a valid client is accepted after the bad one; close test added
- Step ID: `STEP 2E`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `crates/rferram-net/src/endpoint.rs`
  - `crates/rferram-net/tests/quic_tests.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
