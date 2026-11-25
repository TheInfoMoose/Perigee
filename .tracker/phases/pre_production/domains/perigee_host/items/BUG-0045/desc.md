## Problem Description
crates/perigee-host/src/backends/broker.rs:596-611 adopts systemd socket-activated FD 3 via UnixListener::from_raw_fd(3) without setting FD_CLOEXEC. Because WorkerCommand::build_with_mode does not close inherited FDs, every unprivileged capture-worker child inherits the broker's listening socket FD 3, allowing same-UID desktop processes to accept/hijack perigee-host connections or hold the socket open across restarts. Fix: set FD_CLOEXEC on FD 3 via libc::fcntl(3, F_SETFD, FD_CLOEXEC) immediately on adoption.

## Reproduction Steps
1. Inspect `crates/perigee-host`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `crates/perigee-host`
