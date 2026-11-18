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
| **Auth** | **Planned** | TLS certificate verification currently accepts any peer certificate and `perigee-host` grants a session on an unauthenticated `ClientHello`; pinned peer authentication and pairing via `simply-transfer-link` are planned for Phase 4. |
| **Capture** | **Stand-in** | Linux (Wayland/X11), Windows, and macOS capturers probe display availability and return constant-filled 64 KiB buffers (stopping the session on capture error); real headless virtual-output and screen-share capture are planned for Phases 5 and 8. |
| **Encode / Decode** | **Stand-in** | Host video encoders slice raw frame bytes to 1–2 KiB without running a codec, and the client forwards the received payload into DecodedFrameSurface undecoded (crates/perigee-client/src/session.rs:218-224); real video encoding and decoding are planned for Phase 5. |
| **Input** | **Stand-in** | Input events travel over QUIC datagrams; Linux Wayland and X11 injectors only log and update counters, while Windows `SendInput` and macOS `CGEventPost` call OS APIs with partial key maps and unscaled pixel coordinates. |
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

> **Warning:** Authentication is not implemented yet. `perigee-host` currently accepts any `ClientHello` without verifying peer identity or credentials. Do not expose the host port on an untrusted network.

By default, `perigee-host` binds only to loopback (`127.0.0.1:4433`). Configure binding via environment variables:

* `PERIGEE_BIND_ADDR`: Socket address and port to bind (default: `127.0.0.1:4433`).
* `PERIGEE_BIND_INTERFACE`: Optional network/VPN interface name (e.g., `wg0`, `tailscale0`, `utun3`). If set, `perigee-host` binds strictly to that interface and exits with an error if the interface cannot be found or has no matching address.

```bash
# Run on loopback (default 127.0.0.1:4433)
cargo run -p perigee-host --locked

# Bind to a specific VPN interface (fails closed if wg0 is absent)
PERIGEE_BIND_INTERFACE=wg0 PERIGEE_BIND_ADDR=0.0.0.0:4433 cargo run -p perigee-host --locked
```

---

## Roadmap

From the Phase 1 audit and evaluation (`docs/audit/13-evaluation.md`):

1. **Phase 2 — Hygiene & Stand-in Cleanup (Complete)**: Delete stale root files, dead OOB/snapshot/elevation/passthrough code, and unused dependencies; enforce fail-closed VPN socket binding, loopback default bind, resilient accept loop, signal handling, and session reaper; label remaining stand-ins and rename the workspace to Perigee.
2. **Phase 3 — CLI Client & Session Lifecycle**: Extract connect and profile persistence logic out of `perigee-ui` into `perigee-client` and `perigee-common`; provide `perigee` and `perigee-host` CLI entrypoints, a complete session state machine, clean teardown, and a real host-to-client loopback integration test.
3. **ST Track & Phase 4 — Authenticated Transport (`simply-transfer-link`)**: Co-develop `simply-transfer-link` in `ST_Rust_Age` and adopt it in Perigee for SSH control-plane pairing, pinned peer certificates, session tickets, and a single authenticated QUIC endpoint.
4. **Phase 5 — Linux / COSMIC Media & Input Pipeline**: Implement real headless virtual-output and login-screen (`cosmic-greeter`) capture, video encoding, client decoding, `wgpu` surface presentation, dynamic client-native output scaling, and Wayland input injection on Linux.
5. **Phase 6 — Clipboard & File Transfer**: Wire bidirectional clipboard synchronization (text, images, files) and verified file transfers over `simply-transfer-link`.
6. **Phase 7 — Two-App Packaging & Hardened Services**: Split packaging into the full Perigee suite and the lightweight `perigee-host` destination package with least-privilege, VPN-bound service definitions across Linux, macOS, and Windows.
7. **Phase 8 — macOS & Windows Media Backends**: Implement real capture, hardware/software encoding, and input injection on macOS and Windows.
8. **Phase 9 — COSMIC-Native GUI**: Rebuild `perigee-ui` as a thin frontend on upstream System76 `libcosmic` and `cosmic-config`.

---

## License

This project is licensed under the **GPL-3.0-only License**.
