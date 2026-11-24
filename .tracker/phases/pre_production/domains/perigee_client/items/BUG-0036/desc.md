## Problem Description
test_should_fallback_to_software_preserves_vaav1dec_on_corrupt_or_orphaned_frames (crates/perigee-client/src/decode.rs:559-565) only tests the pure boolean helper should_fallback_to_software. It does not exercise GstreamerAv1Decoder::decode, DecodeError classification (TimeoutOrInput vs Pipeline), or has_decoded_frame state updates.

## Reproduction Steps
1. Inspect `crates/perigee-client`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-client`
