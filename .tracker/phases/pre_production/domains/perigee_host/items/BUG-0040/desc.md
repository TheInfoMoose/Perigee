## Problem Description
`output_info_to_monitor_info` (`crates/perigee-host/src/backends/wayland_shm.rs:233-260`) stores `zxdg_output_v1` logical `x`/`y` alongside physical `wl_output` `width`/`height` in `MonitorInfo`. `compute_virtual_desktop_bounds` and `map_output_pointer_to_abs` (`crates/perigee-host/src/backends/uinput.rs:139-221`) add physical `width`/`height` to logical `x`/`y`, inflating virtual desktop bounds and misrouting `EV_ABS` pointer events whenever any output has `scale_factor != 1.0`. Also note `screen_sink` (`daemon.rs:262-264`) updates `screen_width`/`screen_height` atomics on mode change without updating `output_layout.monitors`.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
