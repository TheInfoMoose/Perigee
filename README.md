# Perigee

<p align="center">
  <img src="packaging/icons/perigee.png" alt="Perigee Logo" width="160" height="160" />
</p>

**Perigee** is a direct point-to-point remote desktop application written in Rust over QUIC (`quinn` + `rustls`). It is designed for VPN-only reach with fail-closed interface binding, a CLI-first and headless-first architecture targeting Linux (Wayland / COSMIC, including unattended access across `cosmic-greeter` and user sessions), macOS, and Windows, and a planned COSMIC-native (`libcosmic`) graphical frontend.

---

## Status

| Capability | Status | Current Implementation |
| :--- | :---: | :--- |
| **Transport** | **Works** | `perigee-net` runs QUIC streams and datagrams over `quinn`, `VpnSocketFactory` binds to a named VPN interface or specific IP and fails closed if missing, and the host accept loop skips failed handshakes and shuts down cleanly on `SIGINT`/`SIGTERM`. |
| **CLI** | **Works** | `perigee-host` takes `--bind` / `--interface` / `--name`; `perigee connect <host[:port]>` opens a session, prints status and frame counts, and disconnects cleanly; `perigee host` runs an embedded host; a loopback test runs the real host against the real client. |
| **Auth** | **Works** | Every session uses mutual TLS 1.3 over QUIC. Each node has a persistent Ed25519 key (`identity.key`, mode 0600); the client pins the host's key (`known_hosts`) and the host admits only client keys listed in `trusted_clients`, so an untrusted peer fails the TLS handshake. Keys can be exchanged by hand or paired in one step over SSH via `simply-transfer-link` (`perigee-host pair` / `perigee pair`). |
| **Capture** | **Works (Linux)** | With `--media live` or `--media follow-seat`, `perigee-host` captures a real Wayland output through `ext-image-copy-capture-v1` (shared memory, output 0, no consent dialog on COSMIC), including the greeter under `follow-seat`. The default `--media standin` and the Windows/macOS capturers still return constant-filled buffers. DMA-BUF, multi-monitor and headless virtual outputs are planned for Phase 5b. |
| **Encode / Decode** | **Stand-in** | Host video encoders slice raw frame bytes to 1–2 KiB without running a codec, and the client forwards the received payload into DecodedFrameSurface undecoded (crates/perigee-client/src/session.rs:218-224); real video encoding and decoding are planned for Phase 5. Live media sends raw frames (`RawBgra`, at most 4 MiB, 10 fps) as a stand-in until Phase 5b's codec. |
| **Input** | **Works (Linux)** | Input events travel over QUIC datagrams. With `--media live` or `follow-seat`, Linux injects them through `/dev/uinput` (virtual keyboard and absolute pointer; one monitor). Under `standin` the Linux injectors only log; Windows `SendInput` and macOS `CGEventPost` call OS APIs with partial key maps. |
| **Clipboard** | **Stand-in** | `perigee-channels` implements `arboard` text and RGBA image read/write helpers with echo suppression, but the clipboard monitor loop is not yet wired into live sessions and file-list clipboard sync is not implemented. |
| **File transfer** | **Planned** | Legacy snapshot-transfer and OOB SSH stubs were removed in Phase 2; verified bidirectional file transfer over `simply-transfer-link` is planned for Phase 6. |
| **Audio** | **Stand-in** | The host audio capturer emits synthetic zero-filled 192-byte chunks every 20 ms and the client buffers them in memory without OS audio capture, Opus encoding, or playback; real audio is deferred until after video and input. |
| **GUI** | **Stand-in** | `perigee-ui` renders the launcher grid and modals using an in-repo software rasterizer (`vendor/libcosmic`) with no remote desktop viewport; extraction of client/profile logic and a rebuild on upstream System76 `libcosmic` are planned for Phases 3 and 9. |
| **Packaging** | **Stand-in** | Linux, macOS, and Windows packaging scripts build combined archives/installers containing both `perigee-ui` and `perigee-host`; splitting into the full app and a lightweight `perigee-host` destination package with hardened non-root service units is planned for Phase 7. |

---

## Build and test

Requires Rust `1.93+` (`edition = "2024"`). On Linux, ensure `pkg-config` and `libxkbcommon-dev` (or your distribution's equivalent) are installed.

Run the workspace verification gates against the locked dependency graph:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

---

## Running the host (development)

> **Note:** Only clients whose node key is in the host's `trusted_clients` file can connect, and the client refuses a host whose key it has not pinned. Keep the host bound to loopback or a VPN interface anyway: handshakes are rate limited per source IP (20 per 60 s) and open connections are capped at 64, but the port still answers anyone who can reach it.

By default, `perigee-host` binds only to loopback (`127.0.0.1:4433`). Configure binding via CLI flags (`--bind`, `--interface`, `--name`, `--config-dir`) or environment variables (CLI flags override environment variables):

* `PERIGEE_BIND_ADDR`: Socket address and port to bind (default: `127.0.0.1:4433`).
* `PERIGEE_BIND_INTERFACE`: Optional network/VPN interface name (e.g., `wg0`, `tailscale0`, `utun3`). If set, `perigee-host` binds strictly to that interface and exits with an error if the interface cannot be found or has no matching address.
* `PERIGEE_CONFIG_DIR`: Directory holding `identity.key`, `trusted_clients` and `known_hosts` (default: `$XDG_CONFIG_HOME/perigee`, else `~/.config/perigee`; `%APPDATA%\Perigee` on Windows).

```bash
# Run on loopback (default 127.0.0.1:4433)
cargo run -p perigee-host --locked

# Bind to a specific VPN interface via CLI flag or env vars (fails closed if wg0 is absent)
cargo run -p perigee-host --locked -- --interface wg0
PERIGEE_BIND_INTERFACE=wg0 PERIGEE_BIND_ADDR=0.0.0.0:4433 cargo run -p perigee-host --locked

# Connect with the CLI client for 10 seconds
cargo run -p perigee-cli --locked -- connect 127.0.0.1:4433 --for 10
```

Trust is set up once per pair of machines:

```bash
# On the host: print its node key and give it to the client
cargo run -p perigee-host --locked -- identity
# On the client: pin that key for the host's address
cargo run -p perigee-cli --locked -- trust add 127.0.0.1:4433 <HOST_KEY>
# On the client: print its node key and give it to the host
cargo run -p perigee-cli --locked -- identity
# On the host: allow that client (takes effect on the next connection)
cargo run -p perigee-host --locked -- trust add <CLIENT_KEY> laptop
```

### Live capture and input (Linux)

Live media is opt-in. Inside a Wayland session (COSMIC, or any compositor with `ext-image-copy-capture-v1`):

```bash
# one frame of output 0 to a file, and a pointer/keyboard check (needs write access to /dev/uinput)
perigee-host snapshot --out /tmp/frame.ppm
perigee-host input-test
# serve this session, then pull one frame from a client
perigee-host --media live --bind 192.0.2.10:4433
perigee connect 192.0.2.10 --dump-frame /tmp/remote.ppm --for 5
```

Frames are raw for now (about 40 MB/s at 10 fps), so use loopback or a LAN.

The packaged service (`perigee-host.service`) runs `perigee-host --media follow-seat` as the `perigee` user with only `CAP_SETUID`, `CAP_SETGID` and `CAP_NET_RAW`. It follows logind's active session on seat0, starting a capture worker as that session's user, so it works at the COSMIC greeter and keeps the remote session open across login and logout. Its keys and trust file live in `/var/lib/perigee`:

```bash
sudo systemctl enable --now perigee-host
sudo -u perigee perigee-host identity --config-dir /var/lib/perigee
sudo -u perigee perigee-host trust add <CLIENT_KEY> laptop --config-dir /var/lib/perigee
# or pair once
sudo -u perigee perigee-host pair --bind 192.0.2.10:4434 --config-dir /var/lib/perigee
```

### Pairing (one copy)

Instead of copying keys both ways, the host can open a one-client pairing window. It listens on TCP only while the window is open (default `127.0.0.1:4434`, 10 minutes); bind it to the address the client can reach, such as a VPN IP.

```bash
# on the host
perigee-host pair --bind 192.0.2.10:4434
# prints the host key and a one-time secret
# on the client
perigee pair 192.0.2.10 <HOST_KEY> <SECRET> --label laptop
perigee connect 192.0.2.10
```

`perigee pair` checks that the host proves it holds `HOST_KEY` before sending the secret, adds this machine to the host's `trusted_clients` as `laptop`, and pins the host in `known_hosts` for `192.0.2.10:4433`. Pins are stored per target, trimmed and lowercased with the default port added, so `host.example.com` and `192.0.2.10` are pinned separately. If the host fails to save the new trust entry, only the host's output shows it; run `perigee-host trust list` to check.

---

## Roadmap

From the Phase 1 audit and evaluation (`docs/audit/13-evaluation.md`):

1. **Phase 2 — Hygiene & Stand-in Cleanup (Complete)**: Delete stale root files, dead OOB/snapshot/elevation/passthrough code, and unused dependencies; enforce fail-closed VPN socket binding, loopback default bind, resilient accept loop, signal handling, and session reaper; label remaining stand-ins and rename the workspace to Perigee.
2. **Phase 3 — CLI Client & Session Lifecycle (Complete)**: Extract connect logic out of `perigee-ui` into `perigee-client`; provide `perigee` and `perigee-host` CLI entrypoints, a complete session state machine, bounded peer data handling, clean teardown, and a real host-to-client loopback integration test.
3. **Phase 4 / 4b — Authenticated Sessions and SSH Pairing (Complete)**: Persistent Ed25519 node identities, mutual TLS with pinned host keys and a host trust file on Perigee's single QUIC endpoint, removal of the in-band auth handshake, and one-step SSH pairing via `simply-transfer-link` (`perigee-host pair` / `perigee pair`) that fills the same trust files.
4. **Phase 5 — Linux / COSMIC Media & Input Pipeline**: Implement real headless virtual-output and login-screen (`cosmic-greeter`) capture, video encoding, client decoding, `wgpu` surface presentation, dynamic client-native output scaling, and Wayland input injection on Linux.
5. **Phase 6 — Clipboard & File Transfer**: Wire bidirectional clipboard synchronization (text, images, files) and verified file transfers over `simply-transfer-link`.
6. **Phase 7 — Two-App Packaging & Hardened Services**: Split packaging into the full Perigee suite and the lightweight `perigee-host` destination package with least-privilege, VPN-bound service definitions across Linux, macOS, and Windows.
7. **Phase 8 — macOS & Windows Media Backends**: Implement real capture, hardware/software encoding, and input injection on macOS and Windows.
8. **Phase 9 — COSMIC-Native GUI**: Rebuild `perigee-ui` as a thin frontend on upstream System76 `libcosmic` and `cosmic-config`.

---

## License

This project is licensed under the **GPL-3.0-only License**.
