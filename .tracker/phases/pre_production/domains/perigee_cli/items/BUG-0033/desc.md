## Problem Description
crates/perigee-cli/src/main.rs:95-97, 281: run_connect returns early into run_connect_window without forwarding or rejecting args.dump_frame, silently ignoring --dump-frame when --window is set. Also calls blocking run_viewer_window directly on the Tokio runtime thread instead of using tokio::task::block_in_place.

## Reproduction Steps
1. Inspect `crates/perigee-cli`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-cli`
