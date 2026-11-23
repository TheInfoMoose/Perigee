## Problem Description
In crates/perigee-host/src/daemon.rs:328-336, TrySendError::Full only re-arms force_keyframe when need_keyframe was true, missing encoder-initiated keyframes (video_msg.is_keyframe == true) and breaking inter-frame reference chains once a stateful codec (AV1) replaces RawVideoEncoder. Also consider dropping before encode when video_tx is full.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
