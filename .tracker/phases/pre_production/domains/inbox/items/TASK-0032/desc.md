## Problem Description
Imported from roadmap step `STEP 2M`: Docs + skills rename. POST_GATE exit 0. Strike 1: README claimed the client decode returns RGBA buffers; correction 1 fixed it to say the payload is forwarded undecoded. Skills renamed, absolute paths gone, verify-crate.sh uses rev-parse --show-toplevel, AGENTS/CONTRIBUTING gates match; README status rows checked against audit 11/13
- Step ID: `STEP 2M`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `README.md`
  - `CONTRIBUTING.md`
  - `AGENTS.md`
  - `.agents/`

## Reproduction Steps
1. Inspect `.tracker`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `.tracker`
