# 02 Protocol and Common audit
Audited HEAD: b9f8909

## Summary
- `rferram-protocol` implements `postcard` serialization and 4-byte little-endian length-prefixed framing with an outer `MAX_WIRE_FRAME_BYTES` (16 MiB) cap (`13/13` tests pass), but leaves inner `Vec`/`String` fields unbounded within that 16 MiB envelope and miscalculates non-byte `Vec` sizes in `estimated_payload_bytes()`.
- Of the 28 leaf sub-variants across `WireMessage`, 8 are neither sent nor handled in production (`ServerHello`, all 4 `TopologyMessage` variants, `SetTargetFps`, `SetQuality`, `KeepAlive`), and 12 more are missing either a live production sender (`AuthRequest`, `BandwidthEstimate`, `Disconnect`, `MouseWheel`, `TouchEvent`, `TextInput`, `ClipboardPayload::Files`, plus unwired channel helpers) or a server handler (`ClientToHostMicrophone`).
- Protocol version negotiation is a stub (`daemon.rs:386` only checks `protocol_version != 0`), and `HandshakeMessage` derives unredacted `fmt::Debug` without `Zeroize`, leaking raw `AuthRequest::credentials` to logs at `daemon.rs:373-376`.
- `NistVault` (`rferram-common`, `3/3` tests pass) delegates storage to `simply_transfer_crypto::SecureStorage`, which on Linux has no OS keyring backend and encrypts its vault file (or `/tmp/simply-transfer-vault` fallback) with a deterministic, zero-entropy key `SHA-256("SimplyTransfer:NIST-SP-800-57:VaultKey:" + service_name + ":" + $USER)`.
- `NistVault::verify_channel_bound_challenge` verifies a client-chosen challenge slice unpacked from `AuthRequest::credentials[128..]` with no server-issued challenge nonce, making Ed25519 channel-bound signatures replayable across connections to the same host certificate.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Wire encode/decode | Real | `crates/rferram-protocol/src/lib.rs:25-43`, `tests/wire_tests.rs:15-45` (`13/13` tests pass via `cargo test -p rferram-protocol --locked`) | R3, R4, R5, R11 |
| Length-prefix bounds | Partial | Outer frame capped at `MAX_WIRE_FRAME_BYTES` (16 MiB) in `crates/rferram-protocol/src/lib.rs:11, 56-68`; inner `Vec`/`String` fields lack per-field bounds and `estimated_payload_bytes` (`:92-153`) counts elements instead of bytes for non-`u8` `Vec`s | R3, R11 |
| Protocol versioning | Stub | `ClientHello::protocol_version` and `ServerHello::server_version` in `crates/rferram-protocol/src/lib.rs:162-171`; no version constant exists, `ServerHello` is never sent, and `crates/rferram-server/src/daemon.rs:386-388` only rejects `protocol_version == 0` | R3, R11 |
| Handshake/auth message design | Partial | `crates/rferram-protocol/src/lib.rs:161-192`; `ServerHello` is unused, `AuthRequest` embeds a client-chosen challenge (`crates/rferram-server/src/daemon.rs:333-348, 412-426`), `ClientHello` alone grants a session (`:379-399`), and `HandshakeMessage` derives unredacted `Debug` (`lib.rs:160`) | R3 |
| Vault storage | Partial | `crates/rferram-common/src/vault.rs:26-69` wraps `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:29-180`; Linux always returns `NoDefaultStore` (`:42-45`) and writes an AES-256-GCM file keyed by `SHA-256(prefix + service + $USER)` (`:110-123`) with `/tmp` fallback (`:89-91`) | R3, R11, R12 |
| Identity key generation | Real | `crates/rferram-common/src/vault.rs:72-99` (`get_or_create_ed25519_identity` generates 32 random bytes, derives `ed25519_dalek::{SigningKey, VerifyingKey}`, zeroizes local seed buffers, and persists via `SecureStorage`) | R3 |
| Credential zeroize/redaction | Partial | `HostCredentials` and `VaultTokenModel` implement `Zeroize`, `Drop`, and redacted `Debug` in `crates/rferram-common/src/models.rs:63-122`; `HandshakeMessage` (`crates/rferram-protocol/src/lib.rs:160-185`) does neither, and `vault.rs:50-51` / `vendor/.../lib.rs:141, 153, 191` leave plaintext secret copies unzeroized | R3 |
| OS/distro detection | Real | `crates/rferram-common/src/env.rs:19-89` (`RuntimeEnvironment::detect` and `parse_os_release`; whole-file substring matching in `:64-87` can misclassify if `PRETTY_NAME` or comments contain distro substrings) | R10, R12 |

### Variant coverage table

Production (`src/`, non-`#[cfg(test)]`) sender and handler coverage for every `WireMessage` sub-variant defined in `crates/rferram-protocol/src/lib.rs`:

| `WireMessage` sub-variant | Sent by (`server` / `client` / `nobody`) | Handled by (`server` / `client` / `nobody`) | Notes |
| :--- | :--- | :--- | :--- |
| `Handshake(HandshakeMessage::ClientHello)` | **client** (`crates/rferram-client/src/session.rs:150-164`) | **server** (`crates/rferram-server/src/daemon.rs:379-399, 558-563`) | Server immediately replies with `AuthResponse { success: true }` without requiring `AuthRequest`. |
| `Handshake(HandshakeMessage::ServerHello)` | **nobody** (only in test mock `crates/rferram-client/tests/e2e_loopback.rs:101`) | **nobody** (`crates/rferram-server/src/daemon.rs:460-462` bails; `crates/rferram-client/src/session.rs:186-190` only logs `?h`) | Dead variant (`PROTO-01`). |
| `Handshake(HandshakeMessage::AuthRequest)` | **nobody** in live client/UI (`RFerramHostDaemon::build_ed25519_auth_request` at `crates/rferram-server/src/daemon.rs:333-349` is only called in tests) | **server** (`crates/rferram-server/src/daemon.rs:400-459, 565-576, 707-750`) | Client never sends `AuthRequest` in production (`PROTO-01`). |
| `Handshake(HandshakeMessage::AuthResponse)` | **server** (`crates/rferram-server/src/daemon.rs:518-523, 586-591, 739-748`) | **client** (`crates/rferram-client/src/session.rs:186-190`; `crates/rferram-ui/src/session_controller.rs:729-750`) | Client only checks `success: true`; ignores `success: false`, `session_token`, `error_message`, `requires_elevation`. |
| `Topology(TopologyMessage::DisplayLayout)` | **nobody** | **nobody** (`crates/rferram-client/src/session.rs:192-194` only logs `?t`; `crates/rferram-server/src/daemon.rs:263` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Topology(TopologyMessage::ResizeRequest)` | **nobody** | **nobody** (`crates/rferram-server/src/daemon.rs:263` `_ => {}`; `crates/rferram-client/src/session.rs:192-194` only logs `?t`) | Dead variant (`PROTO-01`). |
| `Topology(TopologyMessage::DisplayAdded)` | **nobody** | **nobody** (`crates/rferram-client/src/session.rs:192-194` only logs `?t`; `crates/rferram-server/src/daemon.rs:263` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Topology(TopologyMessage::DisplayRemoved)` | **nobody** | **nobody** (`crates/rferram-client/src/session.rs:192-194` only logs `?t`; `crates/rferram-server/src/daemon.rs:263` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Video(VideoFrameMessage)` | **server** (`crates/rferram-server/src/daemon.rs:182, 630`) | **client** (`crates/rferram-client/src/session.rs:195-254`; `crates/rferram-ui/src/session_controller.rs:412-446, 751-778`) | Active in production loop. |
| `Audio(AudioFrameMessage)` (`HostToClientSpeaker`) | **server** (`crates/rferram-server/src/audio.rs:51-60, 85`, `daemon.rs:630`) | **client** (`crates/rferram-client/src/session.rs:255-272` -> `audio.rs:102-143`) | Active in production loop. |
| `Audio(AudioFrameMessage)` (`ClientToHostMicrophone`) | **client** (unwired helper `crates/rferram-client/src/session.rs:292-308` -> `audio.rs:163-182`; zero callers in repo) | **nobody** (`crates/rferram-server/src/daemon.rs:263` hits `_ => {}`) | Never called on client; silently dropped on server (`PROTO-01`). |
| `Input(InputEvent::MouseMove)` | **client** (`crates/rferram-client/src/session.rs:135, 311-321`; helper has no callers outside tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Unwired from UI (`PROTO-01`). |
| `Input(InputEvent::MouseButton)` | **client** (`crates/rferram-client/src/session.rs:135, 324-340`, `crates/rferram-ui/src/session_controller.rs:198-205`; called only in tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Unwired from UI (`PROTO-01`). |
| `Input(InputEvent::MouseWheel)` | **nobody** (no helper on `ClientSessionPipeline`; only in tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Never sent by client (`PROTO-01`). |
| `Input(InputEvent::KeyEvent)` | **client** (`crates/rferram-client/src/session.rs:135, 343-359`; helper has no callers outside tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Unwired from UI (`PROTO-01`). |
| `Input(InputEvent::TouchEvent)` | **nobody** (no helper on `ClientSessionPipeline`; only in tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Never sent by client (`PROTO-01`). |
| `Input(InputEvent::TextInput)` | **nobody** (no helper on `ClientSessionPipeline`; only in tests) | **server** (`crates/rferram-server/src/daemon.rs:213-215` -> `backends/{linux_input,windows,macos}.rs`) | Never sent by client (`PROTO-01`). |
| `Control(ControlMessage::FrameAck)` | **client** (`crates/rferram-client/src/session.rs:247-252`) | **server** (`crates/rferram-server/src/daemon.rs:217-227`) | Sent as QUIC datagram (`bridge.rs:39`). |
| `Control(ControlMessage::LossReport)` | **client** (`crates/rferram-client/src/session.rs:209-213`) | **server** (`crates/rferram-server/src/daemon.rs:228-234`), **client** (logs at `session.rs:278-280`) | Sent over QUIC uni stream (`bridge.rs:55-58`). |
| `Control(ControlMessage::BandwidthEstimate)` | **nobody** (only in tests) | **server** (`crates/rferram-server/src/daemon.rs:235-247`), **client** (`crates/rferram-client/src/session.rs:274-277`, `crates/rferram-ui/src/session_controller.rs:713-728`) | Handled on both sides, never sent (`PROTO-01`). |
| `Control(ControlMessage::SetTargetFps)` | **nobody** | **nobody** (`crates/rferram-server/src/daemon.rs:256` `_ => {}`; `crates/rferram-client/src/session.rs:281` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Control(ControlMessage::SetQuality)` | **nobody** | **nobody** (`crates/rferram-server/src/daemon.rs:256` `_ => {}`; `crates/rferram-client/src/session.rs:281` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Control(ControlMessage::KeepAlive)` | **nobody** | **nobody** (`crates/rferram-server/src/daemon.rs:256` `_ => {}`; `crates/rferram-client/src/session.rs:281` `_ => {}`) | Dead variant (`PROTO-01`). |
| `Control(ControlMessage::Disconnect)` | **nobody** (only in tests `daemon.rs:1090`, `wire_tests.rs:316`) | **server** (`crates/rferram-server/src/daemon.rs:248-255`; ignored by client at `session.rs:281`) | Client `stop()` / `disconnect_profile()` never sends `Disconnect` (`PROTO-01`). |
| `Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Text))` | **server/client** (unwired helper `crates/rferram-channels/src/clipboard.rs:56, 85-88`; zero callers in repo) | **server** (`crates/rferram-server/src/daemon.rs:258, 656, 698, 798` -> `clipboard.rs:138-150`), **client** (`crates/rferram-ui/src/session_controller.rs:469-470, 779-794`; only logged in `session.rs:283-285`) | `start_clipboard_monitor` is never called (`PROTO-01`). |
| `Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Image))` | **server/client** (unwired helper `crates/rferram-channels/src/clipboard.rs:56, 107-115`; zero callers in repo) | **server** (`crates/rferram-server/src/daemon.rs:258, 656, 698, 798` -> `clipboard.rs:151-176`), **client** (`crates/rferram-ui/src/session_controller.rs:469-470, 779-794`) | `start_clipboard_monitor` is never called (`PROTO-01`). |
| `Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Files))` | **nobody** (`crates/rferram-channels/src/clipboard.rs:75-120` only polls text and images) | **server/client** (stubbed: `crates/rferram-channels/src/clipboard.rs:177-184` only records into `last_sent_content`) | Never sent; handler does not touch OS clipboard (`PROTO-01`). |
| `Channel(VirtualChannelMessage::SimplyTransfer)` | **server/client** (unwired helper `crates/rferram-channels/src/snapshot_transfer.rs:55, 106-109`; called only in tests) | **server/client** (`crates/rferram-channels/src/lib.rs:56-58` -> `snapshot_transfer.rs:115-149`) | Helper never called outside `rferram-channels` tests (`PROTO-01`). |
| `Channel(VirtualChannelMessage::DevicePassthrough)` | **server/client** (unwired helper `crates/rferram-channels/src/passthrough.rs:68, 83-85`; called only in tests) | **server/client** (`crates/rferram-channels/src/lib.rs:59-63` -> `passthrough.rs:90-111`) | Helper never called outside `rferram-channels` tests (`PROTO-01`). |
| `Channel(VirtualChannelMessage::RequestElevation)` | **client** (unwired helper `crates/rferram-channels/src/elevation.rs:102, 111-112`; called only in tests) | **server/client** (`crates/rferram-channels/src/lib.rs:64-66` -> `elevation.rs:118-128`) | Helper never called outside `rferram-channels` tests (`PROTO-01`). |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-PROTO-1: some `WireMessage` variants are never sent or never handled (verify with the table in item 3). | Confirmed | `crates/rferram-protocol/src/lib.rs:161-431`, `crates/rferram-server/src/daemon.rs:256, 263, 460`, `crates/rferram-client/src/session.rs:186-194, 281-286` (8 sub-variants neither sent nor handled; 12 more missing a live sender or handler) | PROTO-01 |
| L-PROTO-2: `decode_length_prefixed` checks `MAX_WIRE_FRAME_BYTES` before allocating (confirm or refute). | Partly | Confirmed for the outer slice check in `crates/rferram-protocol/src/lib.rs:60-63`, but stream callers allocate up to `MAX_WIRE_FRAME_BYTES + 4` via `read_to_end` before calling `decode_length_prefixed` (`crates/rferram-net/src/manager.rs:108-110`), `postcard::from_bytes` can allocate 128 MiB for `Vec<u64>` varints inside a 16 MiB frame, and `estimated_payload_bytes` (`lib.rs:104-105, 124, 136`) counts elements rather than bytes | PROTO-03 |
| L-PROTO-3: version negotiation is only `"version != 0"` (`daemon.rs:379-399`). | Confirmed | `crates/rferram-server/src/daemon.rs:386-388` rejects only `protocol_version == 0` and never sends `ServerHello` | PROTO-02 |
| L-COM-1: `SecureStorage` fallback key = `SHA-256(fixed prefix + service + $USER)`, fallback dir may be in temp (`vendor crypto lib.rs:89, :109-121`). | Confirmed | `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:42-45, 89-92, 110-123` (Linux always uses the file vault, keyed deterministically from `$USER` and falling back to `/tmp/simply-transfer-vault`) | COM-01 |
| L-COM-2: the Ed25519 challenge is chosen by the client, so a signature is replayable (`vault.rs:116, daemon.rs:419`). | Confirmed | `crates/rferram-common/src/vault.rs:116-136`, `crates/rferram-server/src/daemon.rs:333-348, 418-426, 719-730` | COM-02 |
| L-COM-3: `HostCredentials` and `VaultTokenModel` implement `Zeroize` + zeroize on `Drop` + redacted `Debug` (`AGENTS.md` rule). | Confirmed | `crates/rferram-common/src/models.rs:70-91, 101-122` implement `Zeroize`, `Drop`, and `"[REDACTED]"` `Debug`; however, `HandshakeMessage` (`crates/rferram-protocol/src/lib.rs:160`) and `NistVault::store_session_token` / `SecureStorage` (`vault.rs:50-51`, `vendor/.../lib.rs:141, 153, 191`) leave secret copies unredacted/unzeroized | COM-03, PROTO-04 |
| L-COM-4: test fixtures use a real local path and plain credentials (`common/src/lib.rs:117, :130-131`). | Confirmed | `crates/rferram-common/src/lib.rs:117, 130-131` and `crates/rferram-protocol/tests/wire_tests.rs:340-341` hardcode `/srv/devshare` and `/home/user` | COM-05 |

## Findings

### PROTO-01: 8 `WireMessage` sub-variants are completely dead in production and 12 more lack a live sender or handler
- Severity: High
- Category: gap
- Requirement: R2
- Evidence: `crates/rferram-server/src/daemon.rs:256, 263` and `crates/rferram-client/src/session.rs:186-194, 281-286`
```rust
                _ => {}
            },
            _ => {}
```
- Confidence: Read
- Impact: `ServerHello`, all 4 `TopologyMessage` variants, and `ControlMessage::{SetTargetFps, SetQuality, KeepAlive}` are neither sent nor handled in production; `AuthRequest`, `BandwidthEstimate`, `Disconnect`, `MouseWheel`, `TouchEvent`, `TextInput`, and `ClipboardPayload::Files` are never sent in production; `ClientToHostMicrophone` is dropped by the server; and virtual-channel/input send helpers are never called outside tests.
- Direction: Wire the required handshake, topology, control, input, and channel variants end-to-end between `rferram-client` and `rferram-server`, and prune or gate unused protocol variants.

### PROTO-02: Protocol version negotiation only checks `protocol_version != 0` and never exchanges `ServerHello`
- Severity: Medium
- Category: stub
- Requirement: R3
- Evidence: `crates/rferram-server/src/daemon.rs:386-388`
```rust
                if protocol_version == 0 {
                    bail!("Unsupported rFerram protocol version 0");
                }
```
- Confidence: Read
- Impact: Any non-zero `protocol_version` (`1..=u32::MAX`) is accepted by the daemon, `rferram-protocol` defines no version constant, and `ServerHello { server_version, .. }` is never transmitted, preventing peer compatibility detection.
- Direction: Define an explicit protocol version constant in `rferram-protocol` and enforce exact or bounded version negotiation during the handshake.

### PROTO-03: Outer `MAX_WIRE_FRAME_BYTES` check does not bound inner `Vec`/`String` allocations or `LossReport` gap expansion
- Severity: Medium
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-protocol/src/lib.rs:135-138` and `crates/rferram-client/src/session.rs:201-205`
```rust
            Self::Control(ctrl) => match ctrl {
                ControlMessage::LossReport { missing_frames } => missing_frames.len(),
                ControlMessage::Disconnect { reason } => reason.len(),
```
- Confidence: Read
- Impact: A 16 MiB frame of 1-byte `postcard` varints in `ControlMessage::LossReport { missing_frames }` deserializes into a 128 MiB `Vec<u64>`, a single forged `VideoFrameMessage::frame_id` causes `(expected..frame_idx).collect::<Vec<u64>>()` (`session.rs:204`) to OOM the client, and `estimated_payload_bytes` undercounts non-`u8` vectors while `encode_length_prefixed` (`lib.rs:47-51`) double-allocates the frame buffer.
- Direction: Enforce per-field length/count limits on untrusted strings and vectors during deserialization, cap `LossReport` range generation, and fix `estimated_payload_bytes` byte calculations.

### PROTO-04: `HandshakeMessage` derives unredacted `fmt::Debug` and does not zeroize `AuthRequest::credentials` or `AuthResponse::session_token`
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-protocol/src/lib.rs:160, 175-184` and `crates/rferram-server/src/daemon.rs:373-376`
```rust
    AuthRequest {
        auth_method: AuthMethod,
        credentials: Vec<u8>,
```
- Confidence: Read
- Impact: `HandshakeMessage` prints raw `credentials` and `session_token` in `fmt::Debug` (logged at `info!` level in `daemon.rs:373-376`) and leaves secret bytes in heap memory after `HandshakeMessage` is dropped.
- Direction: Implement `Zeroize`/`ZeroizeOnDrop` and a custom redacted `fmt::Debug` for `HandshakeMessage` (matching `HostCredentials` and `VaultTokenModel`).

### PROTO-05: `postcard` enums lack explicit discriminant pinning or `#[non_exhaustive]` guards
- Severity: Low
- Category: hygiene
- Requirement: R11
- Evidence: `crates/rferram-protocol/src/lib.rs:72-87, 188-192`
```rust
pub enum AuthMethod {
    Ed25519Signature,
    VaultToken,
```
- Confidence: Read
- Impact: Because `postcard` serializes enum variants by declaration-order varint index and version negotiation is unimplemented (`PROTO-02`), inserting or reordering variants in any of the 17 protocol enums silently reinterprets wire messages between mismatched builds.
- Direction: Pair strict wire version pinning (`PROTO-02`) with golden serialized byte-vector tests for every protocol enum discriminant.

### COM-01: `SecureStorage` encrypts the local vault with a deterministic zero-entropy key derived from `$USER` and falls back to `/tmp`
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:89-92, 114-119`
```rust
        hasher.update(b"SimplyTransfer:NIST-SP-800-57:VaultKey:");
        hasher.update(self.service_name.as_bytes());
        hasher.update(b":");
```
- Confidence: Read
- Impact: On Linux, `set_credential_store()` always returns `Err(NoDefaultStore)` (`lib.rs:42-45`), and on all OSes `set_secret` (`lib.rs:190-192`) writes every secret to `vault_*.enc` (or `/tmp/simply-transfer-vault` if `$HOME/.simply-transfer` fails) encrypted with `SHA-256("SimplyTransfer:NIST-SP-800-57:VaultKey:" + service_name + ":" + $USER)`, allowing any process with file read access to decrypt stored Ed25519 private keys and session tokens.
- Direction: Use a real OS keyring backend on Linux (`secret-service` / `keyutils`), never fall back to shared `/tmp`, and require user/daemon secret entropy rather than `$USER` for file-based vault keys.

### COM-02: Ed25519 channel-bound challenge verification uses a client-chosen challenge with no server nonce
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-common/src/vault.rs:116-120` and `crates/rferram-server/src/daemon.rs:418-426`
```rust
                    let signature = &credentials[64..128];
                    let challenge = &credentials[128..];
```
- Confidence: Read
- Impact: Instead of signing a server-generated random `auth_challenge` from `ServerHello`, the client chooses `challenge` and embeds it inside `AuthRequest::credentials[128..]`, allowing any captured `AuthRequest` to be replayed against the same host certificate.
- Direction: Require the server to generate a fresh per-connection cryptographic random challenge in `ServerHello` and verify the client's signature against that stored server challenge exactly once.

### COM-03: `NistVault` and `SecureStorage` leave intermediate plaintext secret allocations unzeroized in memory
- Severity: Medium
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-common/src/vault.rs:49-52` and `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:190-193`
```rust
        let json = serde_json::to_string(token)?;
        self.storage.set_secret(&key, json)?;
        Ok(())
```
- Confidence: Read
- Impact: While `HostCredentials` and `VaultTokenModel` zeroize on drop (`models.rs:70-122`), `SecureStorage::set_secret` clones the secret `String` into an unzeroized `HashMap<String, String>` (`lib.rs:191`) and `serde_json::to_vec` buffer (`lib.rs:153`), and `derive_vault_key` (`lib.rs:136, 154`) leaves the 32-byte AES key on the stack without zeroizing.
- Direction: Wrap vault map values and temporary JSON/key buffers in `Zeroizing<T>` so plaintext secrets and AES keys are wiped on all return paths.

### COM-04: `VaultTokenModel::expires_at` is never checked against wall-clock time and token comparison is non-constant-time
- Severity: Medium
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-common/src/vault.rs:56-62` and `crates/rferram-server/src/daemon.rs:444-449`
```rust
                    if stored.session_token != presented_token {
                        bail!("Presented VaultToken does not match stored NistVault token");
                    }
```
- Confidence: Read
- Impact: Expired session tokens (`0 < expires_at < now`) remain valid indefinitely because `NistVault::get_session_token` does not check `expires_at` and `daemon.rs:447` only rejects `expires_at == 0`, while `!=` leaks token prefix timing.
- Direction: Enforce `expires_at > current_unix_timestamp` inside `NistVault::get_session_token` and compare session tokens in constant time.

### COM-05: Test fixtures in `rferram-common` and `rferram-protocol` hardcode `/srv/devshare` and persist a test vault file in `$HOME/.simply-transfer`
- Severity: Low
- Category: sensitive-data
- Requirement: R7
- Evidence: `crates/rferram-common/src/lib.rs:22, 117` and `crates/rferram-protocol/tests/wire_tests.rs:340-341`
```rust
            vpn_binding: Some("wg0".to_string()),
            folder_shares: vec!["/srv/devshare".to_string()],
            ..Default::default()
```
- Confidence: Ran (`cargo test -p rferram-common --locked`)
- Impact: Test fixtures embed the real host path `/srv/devshare` (`common/src/lib.rs:117`, `wire_tests.rs:341`), and `test_ed25519_channel_bound_challenge_roundtrip` (`common/src/lib.rs:21-77`) creates `$HOME/.simply-transfer/vault_test_rferram_common.enc` without removing the vault file after deleting its keys.
- Direction: Replace `/srv/devshare` in tests with neutral placeholder paths and isolate vault unit tests to a temporary directory that is cleaned up on drop.

### COM-06: `rferram-common` mixes app-specific UI profile state, unused `HostCredentials`, and transport auth primitives overlapping `simply-transfer-link`
- Severity: Medium
- Category: structure
- Requirement: R11
- Evidence: `crates/rferram-common/src/models.rs:9-28, 64-68` and `crates/rferram-common/src/vault.rs:11, 30-34`
```rust
impl Default for NistVault {
    fn default() -> Self {
        Self::new("com.theinfomoose.rferram")
```
- Confidence: Read
- Impact: `HostCredentials` (`models.rs:64-91`) has zero callers outside `rferram-common`'s own unit test, `ConnectionProfile` (`models.rs:9-60`) mixes UI grouping/audio toggles with network target fields, and `NistVault` duplicates Ed25519 identity and channel-binding logic planned for `simply-transfer-link`.
- Direction: Delegate peer identity, pairing, and channel-binding verification to `simply-transfer-link` / `simply-transfer-crypto` and keep `rferram-common` restricted to shared rFerram session/profile configuration.

### COM-07: `RuntimeEnvironment::parse_os_release` matches substrings across the entire `/etc/os-release` text instead of parsing key-value lines
- Severity: Low
- Category: hygiene
- Requirement: R12
- Evidence: `crates/rferram-common/src/env.rs:64-75`
```rust
        let lower = os_release.to_ascii_lowercase();
        if lower.contains("arch") || lower.contains("cachyos") || lower.contains("manjaro") {
            return PlatformDistribution::LinuxArch;
```
- Confidence: Read
- Impact: Any `/etc/os-release` file containing `"arch"` (such as an architecture note or URL) or `"pop"` in `PRETTY_NAME`, `HOME_URL`, or comments is misclassified as `LinuxArch` or `LinuxDebian` before `ID=` / `ID_LIKE=` keys are inspected.
- Direction: Parse `ID` and `ID_LIKE` key-value lines from `/etc/os-release` and match exact distribution tokens.

## Open questions for Claude
- When `simply-transfer-link` replaces bespoke handshake/auth glue (`R11`), should `HandshakeMessage::{ServerHello, AuthRequest, AuthResponse}` and `NistVault` channel-binding helpers be removed from `rferram-protocol` / `rferram-common` in favor of `simply-transfer-link` session tickets, leaving only media/topology/input/channel negotiation in `ClientHello`?
