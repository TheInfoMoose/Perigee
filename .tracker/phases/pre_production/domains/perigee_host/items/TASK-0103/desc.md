## Problem Description
User Step 5R over LAN: follow-seat sessions delivered 3-4 frames in 5 s on the user desktop (fps 0.0), and at the greeter the first (placeholder) frame arrived only after ~5 s. Suspected: ext-image-copy-capture-v1 lets the compositor hold captures until the source changes (static screen), and loginctl may block during the logout transition. Also frames are decimated twice (worker 8 MiB fit, then RawVideoEncoder 4 MiB) → 1280x360 from 5120x1440. Suggested: measure capture vs pipe vs encode time; send the worker frame at the wire budget once; repeat the last frame when the compositor holds captures.
Found during: User Step 5R (Phase 5a).

**Phase 5b review (2026-11-22):** fitting once at 4 MiB (deferred STEP 5BH) raises the 5120x1440 wire frame from 1,843,200 to 3,275,520 bytes (1.78x, about 262 Mbit/s at 10 fps). It improves resolution and can lower frame rate on a slow link. Get real send timing before landing it.

## Reproduction Steps
perigee connect <host> --for 5 against perigee-host --media follow-seat with a static desktop.

## Fix Log & Agent Notes
