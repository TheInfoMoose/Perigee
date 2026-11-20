## Problem Description
P4-04. `crates/perigee-ui/src/` has no view showing the node key or editing known_hosts/trusted_clients; `session_controller.rs:355-358` only loads them. A GUI-only user can't finish trust setup without the perigee CLI.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (planned work)

## Fix Log & Agent Notes
