## Problem Description
`build_follow_seat_media` (`crates/perigee-host/src/daemon.rs:288-315`) never calls `UinputInjector::set_output_layout`, unlike `build_live_media` (`daemon.rs:256-260`). In `--media follow-seat` mode, `UinputInjector::output_layout` remains empty, so `EV_ABS` pointer events fall back to single-monitor `move_to` across the entire compositor desktop rather than targeting `--output <INDEX>` or multi-monitor layouts.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
