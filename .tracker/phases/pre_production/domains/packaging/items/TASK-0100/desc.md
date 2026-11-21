## Problem Description
`packaging/linux/perigee-host.service` grants CAP_SETUID/CAP_SETGID so `crates/perigee-host/src/backends/worker.rs` can start the capture worker as the session user. CAP_SETUID also allows setuid(0), so a compromise of the network-facing daemon is root-equivalent. Suggested fix: a seccomp filter (SystemCallFilter with setuid/setresuid argument limits is not expressible in systemd; use a small root-owned spawn broker, or libseccomp rules rejecting uid 0) or move spawning into a privileged helper.
Found during: Phase 5a review (5M/5O).

## Reproduction Steps
n/a (security review)

## Fix Log & Agent Notes
