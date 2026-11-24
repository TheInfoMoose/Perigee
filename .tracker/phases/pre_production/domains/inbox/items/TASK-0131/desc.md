## Problem Description
Cargo.toml, Cargo.lock, crates/perigee-host/Cargo.toml, and crates/perigee-client/Cargo.toml have uncommitted gstreamer/winit dependency additions while HEAD (940cc49b) commits encoder_gst.rs, decode.rs, and window.rs. A clean checkout of HEAD fails to compile.

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
