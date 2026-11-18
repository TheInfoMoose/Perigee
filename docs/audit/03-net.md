# 03 Net Transport audit
Audited HEAD: e842815

## Summary
- `PeerCertChannelVerifier` (`crates/rferram-net/src/endpoint.rs:222-255`) unconditionally returns `ServerCertVerified::assertion()` and `HandshakeSignatureValid::assertion()` from `verify_tls12_signature` and `verify_tls13_signature` without verifying the TLS `CertificateVerify` handshake signature, allowing an active MITM to replay a legitimate server's public `CertificateDer` (matching `peer_cert_sha256`) without possessing its private key.
- `QuicServerListener::bind` (`endpoint.rs:120`) generates a fresh ephemeral self-signed certificate on every start so no stable pin exists across restarts, and `QuicServerListener::accept` (`endpoint.rs:174-194`) returns `None` after any single failed client handshake, causing `rferram-server`'s `while let Some(conn) = listener.accept().await` loop (`main.rs:40`) to exit on the first bad packet or ALPN mismatch.
- `VpnSocketFactory::create_bound_udp_socket` (`socket.rs:89-135`) enables `SO_REUSEADDR` and `SO_REUSEPORT` on all sockets and silently falls back to binding `bind_addr` (`0.0.0.0` / `[::]`) whenever a configured `VpnInterfaceBinding::InterfaceName` cannot be bound via `SO_BINDTODEVICE` or resolved via `if-addrs`, while `rferram-server` binds `0.0.0.0:4433` on all interfaces by default (`main.rs:25-29`).
- Neither `QuicConnectionManager`, `QuicClientTransport`, nor `QuicServerListener` ever calls `connection.close(...)`, `endpoint.close(...)`, or `endpoint.wait_idle()`; `QuicClientTransport::connect` (`endpoint.rs:79`) creates and drops a new `quinn::Endpoint` per connection, and `start_metrics_monitor` (`manager.rs:132`) spawns a detached task with no abort handle.
- `QuicTransportConfig` (`lib.rs:20-28`) never reads `enable_datagrams` or `initial_rtt_ms`, never applies `max_concurrent_bidi_streams` or `max_concurrent_uni_streams` on the server (`endpoint.rs:145-152`), and `rferram-net` hardcodes `rustls`'s `ring` provider while duplicating QUIC certificate-pinning logic planned for `simply-transfer-link`.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Client connect | Partial | `crates/rferram-net/src/endpoint.rs:33-102` connects over UDP + `quinn` with `rferram-v1` ALPN, but creates a new `Endpoint` per connect (`:79`), disables TLS signature verification (`:68-69, 238-254`), and ignores `enable_datagrams`/`initial_rtt_ms` | R1, R2, R3 |
| Server listen/accept | Partial | `crates/rferram-net/src/endpoint.rs:114-195` and `crates/rferram-server/src/main.rs:25-61`; defaults to `0.0.0.0:4433`, omits stream limits on server `TransportConfig` (`:145-152`), and `accept()` returns `None` on one failed handshake (`:188-193`) | R1, R3, R7, R8 |
| TLS server auth | Stub | `crates/rferram-net/src/endpoint.rs:222-263`; `PeerCertChannelVerifier` returns `ServerCertVerified::assertion()` (`:235`) and `HandshakeSignatureValid::assertion()` (`:244, 253`) without checking pins or TLS handshake signatures | R3 |
| Cert identity persistence/pinning | Missing | `crates/rferram-net/src/endpoint.rs:120-126` generates a new self-signed cert via `rcgen::generate_simple_self_signed` on every `bind()` with no vault storage or client pin store | R3, R11 |
| Datagrams | Real | `crates/rferram-net/src/manager.rs:52-81` (`send_datagram`, `recv_datagram`) tested in `tests/quic_tests.rs:210-319`, though `QuicTransportConfig::enable_datagrams` (`lib.rs:25`) is never read | R2, R11 |
| Uni/bi streams | Partial | `crates/rferram-net/src/manager.rs:84-125`; `send_uni_message` opens a new stream per message (`:96`), server ignores `max_concurrent_{bidi,uni}_streams` (`endpoint.rs:145-152`), and `QuicConnectionManager` lacks an `accept_bi` wrapper (`daemon.rs:781`, `session_controller.rs:459`) | R2, R4, R5, R11 |
| VPN interface binding (Linux) | Partial | `crates/rferram-net/src/socket.rs:104-121, 134`; tries `SO_BINDTODEVICE` (`bind_device`), falls back to `resolve_interface_ip`, and silently falls through to `bind_addr` (`0.0.0.0`) if the interface is missing or lacks an IP | R1, R12 |
| VPN interface binding (Windows) | Partial | `crates/rferram-net/src/socket.rs:122-130, 134`; binds via `resolve_interface_ip` (`if-addrs`) without `IP_UNICAST_IF` and silently falls through to `bind_addr` (`0.0.0.0`) if unresolved | R1, R12 |
| VPN interface binding (macOS) | Partial | `crates/rferram-net/src/socket.rs:122-130, 134`; binds via `resolve_interface_ip` (`if-addrs`) without `IP_BOUND_IF` and silently falls through to `bind_addr` (`0.0.0.0`) if unresolved | R1, R12 |
| Metrics | Partial | `crates/rferram-net/src/metrics.rs:9-26` and `manager.rs:60-63, 74-79, 99-101, 111-113, 128-147`; tracks RTT, datagrams, and uni/dgram bytes, but `estimated_bandwidth_kbps` (`metrics.rs:16`) is never updated, bidi streams are uncounted, and the monitor task is detached | R2, R7 |
| Connection close | Missing | No call to `quinn::Connection::close` exists in `crates/rferram-net` or downstream crates; disconnect relies on drop or idle timeout | R7 |
| Endpoint shutdown | Missing | No call to `quinn::Endpoint::close` or `wait_idle` exists anywhere; `QuicClientTransport::connect` (`endpoint.rs:79-87`) drops its `Endpoint` immediately after connecting | R7 |

### Test coverage (`cargo test -p rferram-net --locked`)

`cargo test -p rferram-net --locked` passed (`7/7` unit tests in `src/`, `5/5` integration tests in `tests/quic_tests.rs`, `0` doc-tests):

| Test | File:Line | What it actually proves | What it misses |
| :--- | :--- | :--- | :--- |
| `metrics::tests::test_network_metrics_loss_calculation` | `src/metrics.rs:32-36` | `calculate_loss_rate(1000, 25) == 2.5` and `0` sent returns `0.0`. | Does not test `NetworkMetrics` atomics or `estimated_bandwidth_kbps`. |
| `socket::tests::test_vpn_interface_name_classification` | `src/socket.rs:147-160` | Prefix/substring matching for `wg*`, `tailscale*`, `utun*`, `tun*`, `*unifi*`, `*wireguard*`. | Pure string check only. |
| `socket::tests::test_discover_vpn_adapters_excludes_loopback` | `src/socket.rs:163-177` | `discover_vpn_adapters()` filters out loopback addresses. | Does not test behavior when no VPN interface exists. |
| `socket::tests::test_create_bound_udp_socket_modes` | `src/socket.rs:180-212` | Binds `127.0.0.1:0` with `DefaultSystemRoute`, `SpecificIp(127.0.0.1)`, and `InterfaceName("lo")`. | Passing `bind_addr = 127.0.0.1:0` masks the fail-open fallback to `bind_addr` when `InterfaceName` fails to resolve (`NET-05`). |
| `manager::tests::test_quic_loopback_handshake_and_datagram` | `src/manager.rs:160-201` | Loopback connect, 1 `InputEvent` datagram, byte/datagram counters, and `peer_cert_sha256()` match. | Does not test teardown, bad signatures, or multiple clients. |
| `manager::tests::test_quic_loopback_length_prefixed_unidirectional_stream` | `src/manager.rs:204-273` | Loopback `send_uni_message` / `recv_uni_message` for `ClientHello` and `VideoFrameMessage`. | Does not test stream exhaustion or bidirectional streams. |
| `manager::tests::test_quic_loopback_combined_datagram_and_uni_stream_with_metrics_monitor` | `src/manager.rs:276-343` | Loopback uni stream + datagram + first RTT sample (`< 5000 ms`) from `start_metrics_monitor`. | Does not verify monitor task termination when the connection closes. |
| `test_rferram_alpn_constant` | `tests/quic_tests.rs:18-20` | `RFERRAM_ALPN == &[b"rferram-v1"]`. | Constant assertion only. |
| `test_peer_cert_channel_verifier_direct_sha256_capture` | `tests/quic_tests.rs:23-44` | `verify_server_cert` records `SHA-256(cert_der)` in `captured_fingerprint()`. | Never calls `verify_tls13_signature`; does not detect that signature verification is a no-op (`NET-01`). |
| `test_tls13_alpn_rferram_v1_negotiation_and_cert_sha256_capture` | `tests/quic_tests.rs:47-146` | Loopback ALPN `rferram-v1`, SNI `"rferram.host"`, `peer_cert_sha256()` capture, and in-memory `NistVault` channel-bound signature check. | Does not test a MITM replaying `cert_der` without the private key (`NET-01`), and pollutes `$HOME/.simply-transfer/vault_test_rferram_net.enc` (`NET-11`). |
| `test_tls13_alpn_mismatch_rejected` | `tests/quic_tests.rs:149-207` | Client with ALPN `incompatible-alpn-v0` fails handshake and `listener.accept().await` returns `None`. | Inadvertently proves `NET-03` (`listener.accept()` returns `None` instead of continuing to accept subsequent valid clients). |
| `test_loopback_datagram_and_length_prefixed_uni_stream_transmission` | `tests/quic_tests.rs:210-319` | Loopback `Input` datagram, `Video` uni stream, and `FrameAck` datagram with metric counters. | Duplicate of `src/manager.rs` unit tests. |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-NET-1: client verifier accepts any cert and returns success from `verify_tls12_signature`/`verify_tls13_signature` without checking (`endpoint.rs:222-255`). | Confirmed | `crates/rferram-net/src/endpoint.rs:222-255` (`verify_server_cert` returns `ServerCertVerified::assertion()` at `:235`; `verify_tls12_signature` at `:244` and `verify_tls13_signature` at `:253` return `HandshakeSignatureValid::assertion()` without verifying `_dss`) | NET-01 |
| L-NET-2: a fresh self-signed cert is generated on every bind (`endpoint.rs:120`), so no stable pin is possible. | Confirmed | `crates/rferram-net/src/endpoint.rs:120-126` (`rcgen::generate_simple_self_signed` runs inside `QuicServerListener::bind` with no persistence) | NET-02 |
| L-NET-3: `accept()` returns `None` after one failed handshake (`endpoint.rs:188-193`), ending the server's `while-let` loop (`server main.rs:40`). | Confirmed | `crates/rferram-net/src/endpoint.rs:174-194` and `crates/rferram-server/src/main.rs:40` (confirmed at runtime by `tests/quic_tests.rs:202-206`) | NET-03 |
| L-NET-4: UDP socket sets `SO_REUSEADDR` and `SO_REUSEPORT` (`socket.rs:89-91`). | Confirmed | `crates/rferram-net/src/socket.rs:89-91` sets `socket.set_reuse_address(true)?` and `let _ = socket.set_reuse_port(true)` on every UDP socket | NET-04 |
| L-NET-5: interface binding silently falls back to the default address when the interface can't be resolved (`socket.rs:77-141`). | Confirmed | `crates/rferram-net/src/socket.rs:104-135` falls through to `socket.bind(&bind_addr.into())?` at `:134` whenever `resolve_interface_ip` returns `None` (and after `bind_device` fails on Linux) | NET-05 |
| L-NET-6: no `connection.close` / `endpoint.close` / `wait_idle` anywhere; client makes a new `Endpoint` per connect (`endpoint.rs:79`). | Confirmed | `crates/rferram-net/src/endpoint.rs:79-101`; workspace-wide grep shows zero calls to `.close(` or `wait_idle` on `quinn::Connection` or `quinn::Endpoint` | NET-07 |
| L-NET-7: `QuicTransportConfig.enable_datagrams` is never read (`lib.rs:20`). | Confirmed | `crates/rferram-net/src/lib.rs:24-27` and `crates/rferram-net/src/endpoint.rs:51-60, 145-152` (`enable_datagrams` and `initial_rtt_ms` are never read anywhere; `max_concurrent_{bidi,uni}_streams` are never read on the server) | NET-08 |
| L-NET-8: metrics monitor task is spawned with no handle (`manager.rs:132`). | Confirmed | `crates/rferram-net/src/manager.rs:128-147` drops the `tokio::spawn` `JoinHandle` and holds a cloned `Connection` until `update_tx.send` fails | NET-09 |
| L-NET-9: default bind is `0.0.0.0:4433` (`server main.rs:25-29`); judge against R1/R8. | Confirmed | `crates/rferram-server/src/main.rs:25-29` binds `0.0.0.0:4433` with `DefaultSystemRoute` and completes TLS 1.3 handshakes with any peer (`endpoint.rs:136`) | NET-06 |

## Findings

### NET-01: `PeerCertChannelVerifier` skips TLS 1.2/1.3 `CertificateVerify` signature verification and accepts any server certificate
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-net/src/endpoint.rs:235, 247-254`
```rust
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
```
- Confidence: Read
- Impact: Because `verify_tls12_signature` (`:238-245`) and `verify_tls13_signature` (`:247-254`) unconditionally return `HandshakeSignatureValid::assertion()` without verifying the handshake signature against `end_entity`'s public key, an active MITM can replay a legitimate host's public `CertificateDer` during the TLS 1.3 handshake without possessing its private key, causing the client to compute the real server's `peer_cert_sha256` while establishing TLS keys with the attacker.
- Direction: Delegate `verify_tls12_signature`, `verify_tls13_signature`, and `supported_verify_schemes` to the active `rustls::crypto::CryptoProvider` and require `verify_server_cert` to match a pinned certificate SHA-256 fingerprint.

### NET-02: `QuicServerListener::bind` generates an ephemeral self-signed certificate on every start and leaves the serialized private key unzeroized
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-net/src/endpoint.rs:120-122`
```rust
        let cert = rcgen::generate_simple_self_signed(vec!["rferram.host".to_string()])?;
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.signing_key.serialize_der();
```
- Confidence: Read
- Impact: Every daemon start generates a brand-new TLS certificate, preventing clients from pinning a stable server certificate across restarts unless bootstrapped by an authenticated control channel, and `key_der: Vec<u8>` is dropped without zeroization after `PrivateKeyDer::try_from(key_der)`.
- Direction: Either bind each ephemeral QUIC certificate SHA-256 into an authenticated SSH control grant (per `simply-transfer-link`) or persist a pinned host certificate, and wrap serialized private key bytes in `Zeroizing`.

### NET-03: `QuicServerListener::accept` returns `None` on a single failed client handshake, terminating `rferram-server`
- Severity: Critical
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-net/src/endpoint.rs:174-194` and `crates/rferram-server/src/main.rs:40`
```rust
                Err(e) => {
                    error!("Failed to complete incoming QUIC handshake: {e}");
                }
```
- Confidence: Ran (`cargo test -p rferram-net --locked -- test_tls13_alpn_mismatch_rejected`)
- Impact: Whenever any incoming client fails the QUIC/TLS handshake (such as an ALPN mismatch, port scan, or dropped packet), `incoming.await` returns `Err`, `accept()` falls through and returns `None`, and `rferram-server`'s `while let Some(conn) = listener.accept().await` loop (`main.rs:40`) exits immediately, allowing a single unauthenticated UDP packet sequence to shut down the host daemon.
- Direction: Loop over `self.endpoint.accept().await` inside `QuicServerListener::accept` (or spawn handshake completion per incoming connection) so `accept()` only returns `None` when the endpoint itself is closed.

### NET-04: `VpnSocketFactory::create_bound_udp_socket` enables `SO_REUSEADDR` and `SO_REUSEPORT` on all UDP sockets
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-net/src/socket.rs:89-91`
```rust
        socket.set_reuse_address(true)?;
        #[cfg(not(windows))]
        let _ = socket.set_reuse_port(true);
```
- Confidence: Read
- Impact: Setting `SO_REUSEADDR` and `SO_REUSEPORT` on the server listener socket (`0.0.0.0:4433`) allows another local process running under the same UID (or any local user on platforms where `SO_REUSEADDR` permits duplicate UDP binds) to bind the same port and intercept or split incoming QUIC packets without triggering `EADDRINUSE`.
- Direction: Remove `set_reuse_address(true)` and `set_reuse_port(true)` from `VpnSocketFactory::create_bound_udp_socket` so UDP port collisions fail fast.

### NET-05: `VpnSocketFactory::create_bound_udp_socket` silently falls back to `bind_addr` (`0.0.0.0`) when a VPN interface cannot be bound or resolved
- Severity: High
- Category: security
- Requirement: R1
- Evidence: `crates/rferram-net/src/socket.rs:104-135`
```rust
        socket.bind(&bind_addr.into())?;
        socket.set_nonblocking(true)?;
```
- Confidence: Read
- Impact: When `VpnInterfaceBinding::InterfaceName(if_name)` is configured, if `resolve_interface_ip` returns `None` (on macOS/Windows, or on Linux after `SO_BINDTODEVICE` fails without `CAP_NET_RAW`), execution falls through to `socket.bind(&bind_addr.into())?` (`0.0.0.0`), silently sending and receiving traffic outside the VPN; additionally, macOS/Windows never set `IP_BOUND_IF`/`IP_UNICAST_IF`, and `SpecificIp` fails if its IP family differs from `bind_addr` (`:81-84, 98-100`).
- Direction: Return an error instead of falling through to `bind_addr` whenever `VpnInterfaceBinding::InterfaceName` cannot bind or resolve the requested interface, derive the socket `Domain` from the resolved IP, and apply `IP_BOUND_IF`/`IP_UNICAST_IF` on macOS/Windows.

### NET-06: `rferram-server` binds `0.0.0.0:4433` on all interfaces by default and completes TLS 1.3 handshakes with unauthenticated peers
- Severity: High
- Category: security
- Requirement: R8
- Evidence: `crates/rferram-server/src/main.rs:25-29`
```rust
    let bind_addr: SocketAddr = std::env::var("RFERRAM_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4433".to_string())
        .parse()?;
```
- Confidence: Read
- Impact: By binding `0.0.0.0:4433` with `VpnInterfaceBinding::DefaultSystemRoute` and `with_no_client_auth()` (`endpoint.rs:136`), the server exposes an active QUIC listener on all physical and virtual interfaces and responds to any network peer advertising `rferram-v1` ALPN with a full TLS 1.3 handshake before checking identity, violating R1 and R8.
- Direction: Bind only to explicitly configured/discovered VPN or local interfaces and gate QUIC session acceptance behind pre-authenticated session tickets or mTLS/pinned peer credentials.

### NET-07: `rferram-net` has no connection close or endpoint shutdown path and creates a new client `quinn::Endpoint` per connect
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-net/src/endpoint.rs:79-101`
```rust
        let mut endpoint = Endpoint::new(
            EndpointConfig::default(),
            None,
```
- Confidence: Read
- Impact: `QuicClientTransport::connect` constructs a fresh `quinn::Endpoint` and UDP socket on every connection attempt and immediately drops the `Endpoint` wrapper at return, while neither `QuicConnectionManager` nor `QuicServerListener` exposes or calls `connection.close(...)`, `endpoint.close(...)`, or `endpoint.wait_idle()`, leaving remote peers waiting for the 10 s idle timeout on disconnect.
- Direction: Retain the `Endpoint` handle and provide explicit `close()` / `shutdown()` methods on `QuicConnectionManager` and `QuicServerListener` that send `APPLICATION_CLOSE` and await `endpoint.wait_idle()`.

### NET-08: `QuicTransportConfig` ignores `enable_datagrams` and `initial_rtt_ms`, and `QuicServerListener::bind` ignores concurrent stream limits
- Severity: Medium
- Category: stub
- Requirement: R2
- Evidence: `crates/rferram-net/src/lib.rs:24-27` and `crates/rferram-net/src/endpoint.rs:51-60, 145-152`
```rust
        let mut transport_config = TransportConfig::default();
        transport_config.max_idle_timeout(Some(
            Duration::from_millis(config.max_idle_timeout_ms).try_into()?,
        ));
```
- Confidence: Read
- Impact: `QuicTransportConfig::enable_datagrams` and `initial_rtt_ms` are never read anywhere in the workspace, `QuicServerListener::bind` (`:145-152`) never applies `max_concurrent_bidi_streams` or `max_concurrent_uni_streams` to the server `TransportConfig`, and `send_uni_message` (`manager.rs:94-103`) allocates and finishes a new unidirectional stream for every single frame.
- Direction: Apply `initial_rtt_ms`, `enable_datagrams` (via `datagram_receive_buffer_size`), `max_concurrent_bidi_streams`, and `max_concurrent_uni_streams` consistently to both client and server `TransportConfig` instances.

### NET-09: `start_metrics_monitor` spawns an untracked background task, while `estimated_bandwidth_kbps` and bidirectional stream bytes are never updated
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-net/src/manager.rs:128-145` and `crates/rferram-net/src/metrics.rs:16`
```rust
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
```
- Confidence: Read
- Impact: `start_metrics_monitor` discards the `tokio::spawn` `JoinHandle`, holds a clone of `quinn::Connection`, and never checks `connection.close_reason()`, while `NetworkMetrics::estimated_bandwidth_kbps` (`metrics.rs:16`) is never written, bidirectional stream traffic bypasses `bytes_sent`/`bytes_received`, and `QuicConnectionManager` lacks an `accept_bidirectional_stream` method (forcing `daemon.rs:781` and `session_controller.rs:459` to access `conn.connection.accept_bi()` directly).
- Direction: Track or abort the metrics monitor task on connection close, add `accept_bidirectional_stream` to `QuicConnectionManager`, and either wire or remove `estimated_bandwidth_kbps`.

### NET-10: `rferram-net` duplicates QUIC certificate-pinning glue from `simply-transfer-link`, uses the `ring` crypto provider instead of `aws-lc-rs`, and declares 3 unused dependencies
- Severity: Medium
- Category: structure
- Requirement: R11
- Evidence: `crates/rferram-net/src/endpoint.rs:64-66, 132-134` and `crates/rferram-net/Cargo.toml:14-18`
```rust
        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
```
- Confidence: Read
- Impact: `rferram-net` hardcodes `rustls::crypto::ring::default_provider()` whereas `russh` and upstream `simply-transfer-link` standardize on `aws-lc-rs` only, duplicates custom QUIC endpoint and `ServerCertVerifier` code that `simply-transfer-link` (Phase 4 STEP 4D/4E) provides via SSH-bootstrapped `UploadGrant` tickets, and declares unused `libc`, `quinn-proto`, and `rferram-common` dependencies in `Cargo.toml`.
- Direction: Standardize on `rustls`'s `aws-lc-rs` provider, reuse `simply-transfer-link` for peer-pinned QUIC session bootstrap, and remove unused dependencies from `crates/rferram-net/Cargo.toml`.

### NET-11: `quic_tests.rs` creates an uncleaned vault file in `$HOME/.simply-transfer` during `cargo test`
- Severity: Low
- Category: hygiene
- Requirement: R7
- Evidence: `crates/rferram-net/tests/quic_tests.rs:114-117`
```rust
    let vault = NistVault::new("test.rferram.net");
    let (signing_key, verifying_key) = vault
        .get_or_create_ed25519_identity("quic_channel_binding_peer")
```
- Confidence: Ran (`cargo test -p rferram-net --locked`)
- Impact: Running `cargo test -p rferram-net --locked` writes `$HOME/.simply-transfer/vault_test_rferram_net.enc` in the user's home directory and never deletes the identity or vault file.
- Direction: Use an ephemeral in-memory `ed25519-dalek` keypair or an isolated `tempfile::tempdir()` vault in `quic_tests.rs`.

## Open questions for Claude
- When `simply-transfer-link` (SSH `st-control/1` + ticketed QUIC data plane on `aws-lc-rs`) is integrated (`R11`), should `rferram-net` remain a separate crate for VPN socket binding (`VpnSocketFactory`) + realtime media/input datagram/stream framing over a `simply-transfer-link`-bootstrapped `quinn::Connection`, or should `VpnSocketFactory` move into `simply-transfer-link`?
