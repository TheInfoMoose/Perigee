## Problem Description
crates/perigee-host/src/backends/uinput.rs:343-357: set_output_layout calls self.set_screen_size(active.width, active.height) before acquiring self.output_layout.lock() to install the new monitors. Because set_screen_size now locks output_layout, this acquires the mutex twice, mutates the stale layout on the first lock, and leaves a race window between updating screen_width/screen_height atomics and updating output_layout under concurrent layout_sink / screen_sink calls. Update screen_width/screen_height atomics while holding the single output_layout lock (or after removing the lock from set_screen_size when BUG-0042 is fixed).

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
