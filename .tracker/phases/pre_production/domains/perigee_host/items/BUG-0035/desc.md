## Problem Description
test_gstreamer_av1_encoder_falls_back_to_software_and_recovers_after_error (crates/perigee-host/src/encoder_gst.rs:742-744) calls fallback_to_software directly instead of exercising the Err(err) if self.use_vaapi branch in VideoEncoder::encode (lines 404-407). Faulting an active pipeline with use_vaapi=true and calling encode() would verify the runtime fallback wiring.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
