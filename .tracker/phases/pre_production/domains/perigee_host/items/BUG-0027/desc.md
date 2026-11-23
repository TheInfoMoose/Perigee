## Problem Description
In crates/perigee-host/src/daemon.rs:693-700, tokio::select! breaks out of the outbound QUIC pump if either outgoing_rx or video_rx returns None. Because outgoing_tx is now held only by HostAudioCapturer, audio stopping independently would close the pump and abort video. Handle closed channels independently until stop_outbound fires or both channels close.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
