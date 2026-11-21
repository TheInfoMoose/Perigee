---
name: perigee-wire-and-quic
description: >-
  Procedures and invariants for implementing and modifying perigee-common,
  perigee-protocol, and perigee-net (NIST vault, Ed25519 mTLS node identities,
  SSH pairing via simply-transfer-link, postcard wire framing, if-addrs VPN
  discovery, socket2 adapter binding, and quinn QUIC streams).
---

# Perigee Wire Protocol, NIST Vault & QUIC Transport Skill

Activate this skill when working on `perigee-common`, `perigee-protocol`, or `perigee-net`.

## 1. `perigee-common` Invariants (`crates/perigee-common`)

- **Secret Zeroization**:
  - `VaultTokenModel` must derive/implement `zeroize::Zeroize`, call `self.zeroize()` in `Drop`, and mask secret fields with `"[REDACTED]"` in custom `fmt::Debug` implementations.
- **NIST Credential Vault (`NistVault`)**:
  - Backed by `simply_transfer_crypto::SecureStorage` (which wraps `keyring-core` with NIST SP 800-57 AES-256-GCM encrypted vault).
  - Always zeroize temporary `String` buffers returned from `self.storage.get_secret(...)` after deserializing.

## 2. `perigee-protocol` Invariants (`crates/perigee-protocol`)

- **Postcard Wire Serialization (No `bincode`)**:
  - Use `postcard::to_stdvec` and `postcard::from_bytes` exclusively.
  - Enforce `MAX_WIRE_FRAME_BYTES = 16 * 1024 * 1024` (16 MiB) before and after serialization to prevent memory exhaustion DoS on malformed frames.
- **Stream vs. Datagram Framing**:
  - **QUIC Datagrams** (`InputEvent`, `ControlMessage::FrameAck`): Use `encode_wire_message` / `decode_wire_message` directly (datagrams are inherently message-framed by QUIC).
  - **QUIC Unidirectional / Bidirectional Streams** (`VideoFrameMessage`, `AudioFrameMessage`, `HandshakeMessage`, `TopologyMessage`, `VirtualChannelMessage`): Use `encode_length_prefixed` / `decode_length_prefixed` (4-byte little-endian `u32` length header followed by the `postcard` payload).

## 3. `perigee-net` Invariants (`crates/perigee-net`)

- **BYON VPN Adapter Discovery & Binding (`VpnSocketFactory`)**:
  - Use `if_addrs::get_if_addrs()` to discover candidate VPN interfaces (`wg*`, `tailscale*`, `utun*`, `tun*`, `unifi*`).
  - On Linux (`#[cfg(target_os = "linux")]`), attempt `socket.bind_device(Some(if_name.as_bytes()))`. If `SO_BINDTODEVICE` fails (e.g., without `CAP_NET_RAW`), resolve the interface's IP via `VpnSocketFactory::resolve_interface_ip` and bind directly to `SocketAddr::new(ip, bind_addr.port())`. If the interface cannot be resolved, fail closed rather than falling back to `bind_addr`.
  - On macOS/Windows, resolve the interface IP via `if-addrs` (or `IP_BOUND_IF` on macOS when interface index is known) and bind non-blocking before passing the `std::net::UdpSocket` to `quinn::Endpoint::new`, failing closed if the interface is missing.
- **QUIC & TLS 1.3 Configuration (`quinn` + `rustls`)**:
  - Enforce `PERIGEE_ALPN = &[b"perigee-v1"]` on both client and server configs.
  - Use `rustls::crypto::aws_lc_rs` as the only crypto provider.
  - Authenticate peers with mutual TLS 1.3 using persistent Ed25519 `NodeIdentity` keys (`<config dir>/identity.key`, 0600 via `fsutil::write_private_file`) and extract peer keys from certificates with `node_key_from_certificate` (webpki SPKI).
  - Bind host endpoints with `QuicServerListener::bind_mtls` using `TrustedClientVerifier` and `PeerAuthorizer`, enforcing `HandshakeLimiter` and `MAX_OPEN_CONNECTIONS` on accept.
  - Connect client endpoints with `QuicClientTransport::connect_mtls` using `PinnedHostVerifier`.
- **Trust Store (`TrustedKeys`)**:
  - Use `TrustedKeys::add` for `trusted_clients` and `TrustedKeys::pin` / `TrustedKeys::remove_label` for `known_hosts` with canonical targets via `perigee_client::canonical_target`.
- **SSH Pairing (`pairing.rs`)**:
  - Run one-client SSH pairing over `simply-transfer-link` via `PairingWindow` (host) and `pair_with_host` (client), reusing the node's Ed25519 key as the SSH key. Never log a pairing secret.

## 4. Verification
After any change to `perigee-common`, `perigee-protocol`, or `perigee-net`, run:
```bash
.agents/skills/perigee-quality-gate/scripts/verify-crate.sh <crate-name>
```
