## Problem Description
packaging/linux/perigee-capture-broker.socket:9 sets DirectoryMode=0750 with no tmpfiles.d rule owning /run/perigee to perigee:perigee. SocketUser/SocketGroup only chown the socket inode, so systemd creates /run/perigee as root:root 0750 and perigee-host.service (User=perigee) fails to connect with EACCES. Fix: change DirectoryMode=0755 (socket inode stays 0660 perigee:perigee) or add a tmpfiles.d rule for /run/perigee.

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
