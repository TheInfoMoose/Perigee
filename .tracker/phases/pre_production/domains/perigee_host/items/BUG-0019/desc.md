## Problem Description
`crates/perigee-host/src/daemon.rs` `serve_connection` queues up to 256 messages. Raw frames of 1.8-3.3 MB allow hundreds of MB and many seconds of stale video per session on a slow link. The STEP 5BG `send_ms` times only the enqueue, so it reads about 0 until the queue fills and can't show the stall TASK-0103 is after. Suggested fix: a latest-frame slot (or depth 1-2) for video, and timing around send_uni_message in the pump.
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
By reading: connect over a link slower than the raw frame rate; Frame timing reports a small send_ms while the queue grows.

## Fix Log & Agent Notes
