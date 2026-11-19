## Problem Description
Imported from roadmap step `STEP 2L`: Packaging/CI rename. POST_GATE exit 0 incl. bash -n on packaging scripts. 18 renames (icons byte-identical, R100) + 9 modified. Normalized diff: only names change, plus WiX ServerExecutable→HostExecutable component ID (GUID kept, no other refs). Packaging builds and WiX not exercised locally
- Step ID: `STEP 2L`
- Command: `/goal`
- Roadmap order: completed
- Target files:
  - `packaging/`
  - `.github/workflows/*.yml`
  - `deny.toml`
  - `.gitignore`

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
