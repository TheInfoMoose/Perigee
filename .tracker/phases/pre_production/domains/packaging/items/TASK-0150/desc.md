## Problem Description
Imported from roadmap step `STEP 10D`: Harden perigee-host.service (drop CAP_SETUID/CAP_SETGID/CAP_KILL, add systemd sandboxing), add perigee-capture-broker.{service,socket}, and wire into package-linux.sh (TASK-0100)
- Step ID: `STEP 10D`
- Command: `/goal`
- Roadmap order: 4
- Target files:
  - `packaging/linux/perigee-host.service`
  - `perigee-capture-broker.service`
  - `perigee-capture-broker.socket`
  - `package-linux.sh`

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
