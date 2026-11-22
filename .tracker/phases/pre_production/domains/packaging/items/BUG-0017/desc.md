## Problem Description
`packaging/linux/package-linux.sh` rpm spec: `%post` and the udev rule need setfacl (STEP 5BB), but only the deb declares `acl`. `%post` hides the failure. Suggested fix: `Requires: acl` in the spec.
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
Install the rpm on a system without setfacl; /dev/uinput gets no perigee ACL entry.

## Fix Log & Agent Notes
