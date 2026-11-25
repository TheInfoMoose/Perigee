## Problem Description
packaging/linux/perigee-host.service:37 sets RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX. If SO_BINDTODEVICE fails under PERIGEE_BIND_INTERFACE, VpnSocketFactory::resolve_interface_ip (crates/perigee-net/src/socket.rs:68-74) calls if_addrs::get_if_addrs(), which opens an AF_NETLINK socket and fails with EAFNOSUPPORT. Fix: add AF_NETLINK to RestrictAddressFamilies.

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
