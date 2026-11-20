## Problem Description
`crates/perigee-cli/src/main.rs` (`format_cert_sha256`, used by `run_connect`) — `perigee connect` still prints the host certificate SHA-256. After Phase 4 users pin the host's 64-hex node key, so the printed value doesn't match anything they set up — suggested fix: print `ClientSession::peer_node_key()` as hex (node_key_to_hex) instead of, or beside, the cert digest.
Found during: Phase 4 planning.

## Reproduction Steps
n/a (planned work). After STEP 4L: `perigee connect 127.0.0.1:4433` prints a cert SHA-256, not the pinned key.

## Fix Log & Agent Notes
