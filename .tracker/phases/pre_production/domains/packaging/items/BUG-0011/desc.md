## Problem Description
`packaging/linux/60-perigee-uinput.rules` sets GROUP="perigee", MODE="0660" on /dev/uinput, but COSMIC/Pop ship `71-uinput-dev-early-creation.rules` (TAG+="uaccess"), so the node carries a logind ACL. On a node with an ACL, MODE group bits only set the ACL mask: getfacl shows `group::---`, `mask::rw-`, `user:<seat user>:rw-`. The perigee-host service (uid/gid 982) gets EACCES and every --media follow-seat session is refused with "failed to create the /dev/uinput devices". Suggested fix: add a named ACL entry, e.g. `RUN+="/usr/bin/setfacl -m g:perigee:rw $devnode"` in the rule (or a tmpfiles.d `a+ /dev/uinput - - - - group:perigee:rw`), and run it from postinst/%post.
Found during: User Step 5R (Phase 5a).

## Reproduction Steps
Install the package on Pop!_OS 24.04 COSMIC; `getfacl /dev/uinput` shows group::---; perigee connect to the service fails.
Workaround: `sudo setfacl -m g:perigee:rw /dev/uinput` (lost on reboot).

## Fix Log & Agent Notes
