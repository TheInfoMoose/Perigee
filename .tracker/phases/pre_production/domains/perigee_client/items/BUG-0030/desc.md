## Problem Description
crates/perigee-client/src/decode.rs:225-247, 343-349: Any single corrupt AV1 payload or orphaned delta frame causes decode_once to time out after 2s, permanently fall back from vaav1dec to software av1dec for the rest of the session, and retry the same bad frame for another 2s (4s total stall blocking ClientSessionPipeline::process_messages). Distinguish hardware pipeline failure from bad/orphaned bitstream input.

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
