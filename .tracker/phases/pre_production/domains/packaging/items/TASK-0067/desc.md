## Problem Description
P4-11. `.github/workflows/release.yml` has no cargo test step; `packaging/linux/package-linux.sh:76-77`, `packaging/macos/package-macos.sh:37,54`, `packaging/windows/perigee.wxs:27-45` and `package-windows.ps1:34,54` ship perigee-ui and perigee-host only, so installs lack the perigee CLI needed for identity/trust/connect.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
n/a (planned work)

## Fix Log & Agent Notes
