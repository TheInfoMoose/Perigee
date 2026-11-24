## Problem Description
crates/perigee-host/src/encoder_gst.rs:70-92, 110-249, 331-350: GstreamerAv1Encoder only checks ElementFactory::find("vaav1enc") at construction and never falls back to av1enc if vaav1enc fails to build, reach Playing, or encode at runtime. In addition, encode_once does not call tear_down_pipeline() on push_buffer or try_pull_sample failure, leaving self.active wedged until frame dimensions change.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
