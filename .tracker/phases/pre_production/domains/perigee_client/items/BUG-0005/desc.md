## Problem Description
P4-05. `crates/perigee-client/src/connect.rs:86-88` looks up the raw target, while resolve_host_addr trims and appends :4433, so a pin for host.example.com:4433 misses a connect to host.example.com. `crates/perigee-net/src/trust.rs:129` replaces any entry with the same key (documented behavior), so pinning one host under its DNS name and IP keeps only the last one. Same filter is used for trusted_clients — decide whether to change it there too.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
perigee trust add host.example.com:4433 <KEY> --config-dir C; perigee trust add 192.0.2.10:4433 <KEY> --config-dir C; perigee trust list --config-dir C → only 192.0.2.10:4433 remains.

## Fix Log & Agent Notes
