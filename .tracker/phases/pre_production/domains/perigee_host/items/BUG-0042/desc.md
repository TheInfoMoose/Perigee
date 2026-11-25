## Problem Description
Phase 9b (STEP 9BA) added OutputLayout::update_active_size inside UinputInjector::set_screen_size (crates/perigee-host/src/backends/uinput.rs:263-274,329-339). Because screen_sink (daemon.rs:473-478) passes encoded video_msg dimensions — which are decimated to <=4 MiB by fit_raw_frame in both --codec raw (default) and --media follow-seat (host.rs:615, any codec) for every >=1080p monitor — set_screen_size overwrites the active monitor's physical width/height in output_layout while leaving scale_factor and sibling (x, y) origins unchanged. logical_monitor_size and compute_virtual_desktop_bounds then shrink the active monitor's logical footprint (e.g. 1920x1080@1x -> 960x540; 3840x2160@2x -> 640x360), making the right/bottom portions unreachable even with normalized [0,1] coordinates. Separate encoded frame pixel size (screen_width/screen_height, used only to normalize pixel-space pointer inputs) from compositor physical output dimensions in output_layout, and add a regression test covering map_output_pointer_to_abs after decimated set_screen_size updates.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
