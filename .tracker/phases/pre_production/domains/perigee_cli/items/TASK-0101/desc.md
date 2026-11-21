## Problem Description
`crates/perigee-cli/src/lib.rs:47-50` USAGE "Host options" lists only --bind/--interface/--name; `perigee host` also takes --config-dir and --media. Separately, `crates/perigee-cli/src/main.rs` run_connect takes the dump path on the first frame even when encode_ppm fails (stand-in frames), so --dump-frame never retries on a later raw frame. Suggested fix: list the flags; only clear dump_path after a successful write.
Found during: Phase 5a review (5J).

## Reproduction Steps
perigee connect <host> --dump-frame /tmp/f.ppm against a standin host

## Fix Log & Agent Notes
