## Problem Description
Imported from roadmap step `STEP 10C`: Add perigee-host capture-broker subcommand and wire --broker-socket / PERIGEE_BROKER_SOCKET through HostOptions, PerigeeHostDaemon, and WorkerCapturer (TASK-0100)
- Step ID: `STEP 10C`
- Command: `/goal`
- Roadmap order: 3
- Target files:
  - `host backends/worker.rs`
  - `daemon.rs`
  - `host.rs`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
