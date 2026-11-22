## Problem Description
Since 6B, WorkerCapturer::capture_next_frame logs "Seat poll failed; retrying at the next poll" once per SEAT_POLL_INTERVAL forever and serves the 64x64 placeholder, with no backoff and no signal to the client. A permanent failure (missing loginctl, worker binary gone after an upgrade) keeps a dead session open and floods the journal. Phase 6 review F3.
Found during: Phase 6 review.

## Reproduction Steps
Run a follow-seat session where WorkerCommand points to a missing binary, or loginctl is denied. The journal gets one warning per second indefinitely and the client only ever sees the placeholder.

## Fix Log & Agent Notes
