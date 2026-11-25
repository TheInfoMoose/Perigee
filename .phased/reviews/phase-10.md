# Phase 10 review

Range: `ea27cadef7935c5ae507354789271f49c987911c..77635fad319aeb12135a59b862dcd6f8981390bf`

## Independent pass

1. **`packaging/linux/perigee-capture-broker.socket:9` — `new defect`**
   - **Trigger:** systemd activates `perigee-capture-broker.socket` on boot and creates `/run/perigee` using `DirectoryMode=0750`, then `perigee-host.service` (`User=perigee`, `Group=perigee`) connects to `/run/perigee/capture-broker.sock`.
   - **Effect:** In system context, `SocketUser=perigee` and `SocketGroup=perigee` chown only the socket inode, not its parent directory. With no `tmpfiles.d` rule owning `/run/perigee` to `perigee:perigee`, systemd creates `/run/perigee` as `root:root` `0750`. `perigee-host.service` has no `CAP_DAC_OVERRIDE` and is not in group `root`, so traversing `/run/perigee` fails with `EACCES` (Permission denied), breaking `follow-seat` capture under systemd.

2. **`crates/perigee-host/src/backends/broker.rs:596-611` — `new defect`**
   - **Trigger:** `perigee-capture-broker.service` starts under systemd socket activation (`LISTEN_FDS=1`, `LISTEN_PID=<pid>`), adopts FD 3 with `UnixListener::from_raw_fd(3)`, and spawns a `capture-worker` child as the active non-root desktop user.
   - **Effect:** Socket-activated FD 3 is inherited across `execve` without `FD_CLOEXEC`, `UnixListener::from_raw_fd(3)` does not set `FD_CLOEXEC`, and `WorkerCommand::build_with_mode` does not close inherited FDs. Every unprivileged `capture-worker` inherits the broker's listening Unix socket FD 3, allowing same-UID desktop session processes to `accept()`/hijack `perigee-host` connections to `capture-broker` or hold the listening socket open across broker restarts.

3. **`packaging/linux/perigee-host.service:37` — `new defect`**
   - **Trigger:** `perigee-host.service` runs with `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX` and `PERIGEE_BIND_INTERFACE=<iface>`, and `SO_BINDTODEVICE` fails so `VpnSocketFactory::create_bound_udp_socket` falls back to `VpnSocketFactory::resolve_interface_ip` (`crates/perigee-net/src/socket.rs:68-74`, which calls `if_addrs::get_if_addrs()`).
   - **Effect:** `getifaddrs()` on Linux opens an `AF_NETLINK` socket, which is blocked (`EAFNOSUPPORT`) by `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`. Because `CAP_NET_RAW` is present, the primary `SO_BINDTODEVICE` path succeeds when the interface exists, so only the IP-resolution fallback is broken.

4. **`crates/perigee-host/src/backends/broker.rs:662-893` — `test defect`**
   - **Trigger:** Unit tests in `broker::tests` run `cargo test -p perigee-host --locked`.
   - **Effect:** Neither `MAX_BROKER_CONCURRENT_WORKERS` slot exhaustion (`"too many active workers"`) nor `bind_broker_listener`'s systemd socket-activation (`LISTEN_FDS=1` / `LISTEN_PID`) adoption path is tested, leaving finding #2 undetected.

### Not checked
- Live systemd activation and COSMIC `seat0` runtime verification (`perigee-host.service`, `perigee-capture-broker.socket`, `perigee-capture-broker.service`, `CapEff`, live multi-monitor pointer mapping under `--codec raw`).
- Executing `packaging/linux/package-linux.sh` to build `.deb`, `.rpm`, or `.AppImage` artifacts.
- Non-Linux (`windows` / `macos`) compilation of `perigee-host`.

<!-- SEALED: Independent pass locked -->

## Contract pass

### Step compliance
- **STEP 10A (`TASK-0147`, closed):** Met. `OutputLayout::update_active_size` removed, `set_screen_size` only updates `screen_width`/`screen_height`, `apply_output_layout` updates layout + atomics under one lock, `resolve_multimonitor_abs` handles normalized and decimated pixel coordinates, and both new tests pass. Resolves `BUG-0042` and `BUG-0043`.
- **STEP 10B (`TASK-0148`, closed):** Met on paper, but introduced Finding #2 (`BUG-0045`, missing `FD_CLOEXEC` on systemd-activated FD 3) and Finding #4 (`TASK-0151`, untested concurrency cap and socket-activation path).
- **STEP 10C (`TASK-0149`, closed):** Met. `--broker-socket`, `PERIGEE_BROKER_SOCKET`, `perigee-host capture-broker`, `PerigeeHostDaemon::with_broker_socket`, and `WorkerCapturer::with_broker_socket` are wired and unit-tested.
- **STEP 10D (`TASK-0150`, closed):** Literally matched the plan, which introduced Finding #1 (`BUG-0044`, `DirectoryMode=0750` causes `EACCES` on `/run/perigee`) and Finding #3 (`BUG-0046`, `RestrictAddressFamilies` omits `AF_NETLINK` for `get_if_addrs` fallback).
- **Deferrals / Amendments:** None in Phase 10.
- **`[USER] 10E`:** Pending; blocked by `BUG-0044` and `BUG-0045` (must fix before live systemd check).

### Finding origins
1. `packaging/linux/perigee-capture-broker.socket:9` (`BUG-0044`) — origin: `plan` (STEP 10D explicitly specified `DirectoryMode=0750` without a `tmpfiles.d` owner rule).
2. `crates/perigee-host/src/backends/broker.rs:596-611` (`BUG-0045`) — origin: `coder` (plan requested `UnixListener::from_raw_fd(3)` without mentioning `FD_CLOEXEC`; coder did not set `FD_CLOEXEC` before spawning unprivileged workers).
3. `packaging/linux/perigee-host.service:37` (`BUG-0046`) — origin: `plan` (STEP 10D explicitly specified `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`).
4. `crates/perigee-host/src/backends/broker.rs:662-893` (`TASK-0151`) — origin: `plan` (STEP 10B `Tests:` omitted concurrency saturation and `LISTEN_FDS` adoption tests).

## Handoff
- **Verdict:** Needs Phase 10b follow-up before `[USER] 10E` (2 high-severity systemd/security defects in `capture-broker`).
- **High:**
  - `BUG-0044` (`packaging`): `perigee-capture-broker.socket` `DirectoryMode=0750` creates `/run/perigee` as `root:root 0750`, causing `EACCES` for `perigee-host.service` (`User=perigee`); change to `DirectoryMode=0755` (or add `tmpfiles.d`).
  - `BUG-0045` (`perigee_host`): `CaptureBroker::bind_broker_listener` adopts FD 3 without `FD_CLOEXEC`, leaking the listening socket FD to every unprivileged `capture-worker`; set `FD_CLOEXEC` via `fcntl`.
- **Low:**
  - `BUG-0046` (`packaging`): Add `AF_NETLINK` to `perigee-host.service` `RestrictAddressFamilies` so `if_addrs::get_if_addrs()` fallback works.
  - `TASK-0151` (`perigee_host`): Add unit tests for `MAX_BROKER_CONCURRENT_WORKERS` saturation and `LISTEN_FDS=1` adoption (`FD_CLOEXEC`).
- **Deferred steps to re-split:** None.
- **Advances ready to close (pending user `approved`):** `BUG-0042`, `BUG-0043` (keep `TASK-0100` open until `BUG-0044`, `BUG-0045`, and `[USER] 10E` complete).
- **Decisions for user:** Confirm whether to close `BUG-0042` and `BUG-0043` now, and whether to bundle `BUG-0044`, `BUG-0045`, `BUG-0046`, and `TASK-0151` into Phase 10b before running `[USER] 10E`.
