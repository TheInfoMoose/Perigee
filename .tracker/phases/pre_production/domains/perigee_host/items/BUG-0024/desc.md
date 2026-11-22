## Problem Description
wayland_shm.rs dispatch_or_cancelled treats a WouldBlock from queue.flush() as success, then polls the connection fd for POLLIN only. If the capture request is still in the outbound buffer, no reply can arrive, so the capture stalls until cancel, or forever in the capture-worker, which never cancels. Unverified against the vendored wayland-client. Phase 6 review F4.
Found during: Phase 6 review.

## Reproduction Steps
Code inspection: the flush match arm for WaylandError::Io WouldBlock falls through to prepare_read and wait_readable_or_cancelled (POLLIN only). Triggering it needs a full compositor send buffer.

## Fix Log & Agent Notes
