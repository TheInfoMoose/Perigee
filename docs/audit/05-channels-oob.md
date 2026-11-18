# 05 OOB SSH and Virtual Channels audit
Audited HEAD: 29f4d03

## Summary
- `OobKeyIngestionManager` (`crates/rferram-channels/src/oob_ssh.rs:46-355`) implements `russh::server::{Server, Handler}` and is constructed inside every `VirtualChannelManager::new()` (`crates/rferram-channels/src/lib.rs:42`), but no code in the workspace ever binds a TCP socket, starts the `russh` server, connects as an SSH client, or consults `OobKeyIngestionManager` during QUIC handshake authentication (`crates/rferram-server/src/daemon.rs:404-427`).
- Within `oob_ssh.rs`, vault failure silently falls back to a deterministic, publicly computable Ed25519 host key seed (`:71-76`), `authorized_peers` initializes empty without reloading `oob_peer_*` entries from `NistVault` (`:80`), and `Handler::data` (`:331-354`) lets any authenticated SSH session register or overwrite the Ed25519 key for any `peer_id`.
- If the `russh` server were bound to a port, any non-app TCP peer or scanner would immediately receive an `SSH-2.0-russh` protocol banner, full KEX + host public key, and `SSH_MSG_USERAUTH_FAILURE` responses (`0 ms` initial delay, `50 ms` subsequent at `oob_ssh.rs:98-99`), violating R8's silent-drop requirement.
- `ClipboardChannelHandler::start_clipboard_monitor` (`crates/rferram-channels/src/clipboard.rs:56-125`) is never called anywhere in the workspace, never polls or sends `ClipboardPayload::Files`, holds both `clipboard` and `last_sent_content` mutexes across `outgoing_tx.send(msg).await` (`:74-118`), and suffers from multiple echo-back paths in `handle_remote_clipboard` (`:133-191`) after its `100 ms` suppression sleep expires between `300 ms` poll ticks.
- `ElevationChannelHandler::handle_elevation_request` (`crates/rferram-channels/src/elevation.rs:118-128`) marks `session_elevated = true` without invoking any OS privilege prompt (`pkexec`, `runas`, `launchctl`), and `DevicePassthroughHandler::handle_device_data` (`crates/rferram-channels/src/passthrough.rs:90-111`) only appends frame metadata to an unbounded `Vec<DevicePassthroughFrame>` (`:29, 109`) with no OS USB/smartcard forwarding.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| OOB server startup | Missing | `crates/rferram-channels/src/oob_ssh.rs:59-107, 269-279` and `crates/rferram-channels/src/lib.rs:42`; `OobKeyIngestionManager` is instantiated inside `VirtualChannelManager::new()`, but `build_russh_server_config()` (`:96`) and `russh::server::Server::run_on_socket` / `run_on_address` are never called outside unit tests (`:533`) | R3, R8, R11 |
| OOB auth | Partial | `crates/rferram-channels/src/oob_ssh.rs:226-239, 284-318`; `auth_publickey_offered` and `auth_publickey` check `is_ssh_key_authorized(public_key)` against the in-memory `authorized_peers` map, but ignore `user` (`:286, 300`), start with an empty map (`:80`), and are never wired into `RFerramHostDaemon::handle_handshake` (`crates/rferram-server/src/daemon.rs:404-427`) | R3, R8 |
| OOB unknown-peer handling (silent vs responsive) | Stub | `crates/rferram-channels/src/oob_ssh.rs:96-107, 272-329`; `russh::server::Config` speaks standard TCP/SSH (`SSH-2.0` banner, KEX, host key) before authentication and replies to unauthorized keys after `0 ms` initial / `50 ms` subsequent delay (`:98-99`), rather than silently ignoring non-counterpart traffic | R8 |
| OOB message handling | Partial | `crates/rferram-channels/src/oob_ssh.rs:320-354`; `channel_open_session` accepts any session channel (`:327`) and `Handler::data` parses `OOB_INGEST_ED25519:<peer_id>:<key>` over raw unframed SSH data chunks without checking whether the caller owns `<peer_id>` | R3, R8 |
| Key registry persistence | Partial | `crates/rferram-channels/src/oob_ssh.rs:80, 138-157, 201-239`; `register_peer_verifying_key` writes `VaultTokenModel` under `oob_peer_{peer_id}` to `NistVault` (`:150-152`), but `with_vault` initializes `authorized_peers` as an empty `HashMap` (`:80`) and never reloads persisted tokens or enforces `expires_at` (`:147`) | R3, R7 |
| Clipboard text | Partial | `crates/rferram-channels/src/clipboard.rs:77-93, 138-150`; reads/writes text via `arboard::Clipboard::{get_text, set_text}`, but `start_clipboard_monitor` (`:56`) is never started in server, client, or UI, `set_text` errors are discarded (`:148`), and empty strings (`!text.is_empty()` at `:78`) are never sent | R4 |
| Clipboard image | Partial | `crates/rferram-channels/src/clipboard.rs:96-119, 151-176`; reads/writes RGBA8 buffers via `arboard::Clipboard::{get_image, set_image}`, but `start_clipboard_monitor` (`:56`) is never started, `set_image` errors are discarded (`:174`), `format` is ignored (`:154`), and `data.len()` vs `width * height * 4` is not validated (`:169-173`) | R4 |
| Clipboard files | Stub | `crates/rferram-channels/src/clipboard.rs:75-120, 177-184`; `start_clipboard_monitor` never reads file lists from the OS clipboard or emits `ClipboardPayload::Files`, and `handle_remote_clipboard` only stores `ClipboardContent::Files(files)` in `last_sent_content` (`:183`) without writing to `arboard` | R4 |
| Echo suppression | Partial | `crates/rferram-channels/src/clipboard.rs:65-120, 134-190`; `is_suppressed` sleeps for `100 ms` (`:188`) against a `300 ms` poll interval (`:65`), and cross-variant state updates (`Files` or `Image` overwriting `last_sent_content` while OS text remains readable at `:77`) let local content bounce back on the next poll tick | R4 |
| Headless clipboard | Real | `crates/rferram-channels/src/clipboard.rs:25, 38-46, 75, 147, 168`; wraps `Option<Clipboard>` inside `Arc<Mutex<Option<Clipboard>>>` and logs `debug!` when `Clipboard::new()` fails so headless callers and tests do not panic | R2, R4 |
| Elevation | Stub | `crates/rferram-channels/src/elevation.rs:39-46, 102-128`; `handle_elevation_request` increments `requests_received` and sets `session_elevated = true` (`:119-120`) without executing `pkexec`, `runas`, `launchctl`, polkit, or UAC | R6, R12 |
| Device passthrough | Stub | `crates/rferram-channels/src/passthrough.rs:28-30, 68-111`; `handle_device_data` hashes payload bytes and pushes `DevicePassthroughFrame` structs into an unbounded in-memory `Vec` (`:109`) with no USB/smartcard/UHID/PC-SC integration | R6, R12 |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-OOB-1: the OOB SSH server is never started; no port is configured anywhere (`oob_ssh.rs`; constructed only in `lib.rs:42`). | Confirmed | `crates/rferram-channels/src/lib.rs:42` and `crates/rferram-channels/src/oob_ssh.rs:59-107, 269-355` (`OobKeyIngestionManager::new()` is called only inside `VirtualChannelManager::new()`; no TCP listener, port config, `run_on_socket`, `run_on_address`, or `russh::client` exists anywhere in the workspace) | OOB-01 |
| L-OOB-2: any authorized peer can register or overwrite a key for any `peer_id` via `OOB_INGEST_ED25519` (`oob_ssh.rs:331-354`). | Confirmed | `crates/rferram-channels/src/oob_ssh.rs:150-156, 284-318, 331-354` (`auth_publickey` ignores `user` and records no session identity; `Handler::data` splits `OOB_INGEST_ED25519:<peer_id>:<key>` and unconditionally overwrites `oob_peer_{peer_id}` in `NistVault` and `authorized_peers`) | OOB-02 |
| L-OOB-3: host key falls back to a fixed, guessable seed when the vault fails (`oob_ssh.rs:72-74`). | Confirmed | `crates/rferram-channels/src/oob_ssh.rs:66-77` (on `Err(_)` from `vault.get_or_create_ed25519_identity("oob_ssh_host")`, `with_vault` silently derives the host `Ed25519Keypair` from `std::array::from_fn(\|i\| (i as u8).wrapping_mul(37).wrapping_add(11))`) | OOB-03 |
| L-OOB-4: the key registry starts empty and vault tokens are not reloaded (`oob_ssh.rs:80`). | Confirmed | `crates/rferram-channels/src/oob_ssh.rs:80, 150-156, 213-239` (`authorized_peers` starts as `HashMap::new()`, never reloads `oob_peer_*` tokens from `NistVault`, never checks `expires_at`, and creates a bootstrap deadlock because `auth_publickey` rejects all keys when `authorized_peers` is empty) | OOB-04 |
| L-OOB-5: against R8, an unknown SSH client still gets a protocol banner and auth replies (`auth_rejection_time` 50 ms, first attempt 0 ms at `:98-99`; any session channel accepted at `:327`). Describe exactly what a non-app peer observes. | Confirmed | `crates/rferram-channels/src/oob_ssh.rs:96-107, 272-354` (a non-app peer connecting over TCP receives the `SSH-2.0-russh` banner, completes KEX receiving the host public key, gets immediate `0 ms` and subsequent `50 ms` `USERAUTH_FAILURE` replies advertising `publickey`, and post-auth gets any `session` channel accepted at `:327`) | OOB-05 |
| L-CLIP-1: `Files` payloads are never sent and incoming file lists are only recorded (`clipboard.rs:177-184`); set errors are discarded (`:148, :174`); lock held while sending; 300 ms poll. | Confirmed | `crates/rferram-channels/src/clipboard.rs:56-125, 148, 174, 177-184` (`start_clipboard_monitor` is never called in the workspace, polls every `300 ms` with no stop handle, never polls/sends `Files`, holds both `clipboard` and `last_sent_content` mutexes across `outgoing_tx.send(msg).await` at `:74-118`, discards `set_text`/`set_image` errors at `:148, :174`, and only sets `*last = ClipboardContent::Files(files)` at `:183`) | CLIP-01, CLIP-02, CLIP-03 |
| L-CLIP-2: echo suppression: can a remote update bounce back as a new local change? Trace it. | Confirmed | `crates/rferram-channels/src/clipboard.rs:65-120, 133-199` (multiple bounce-back paths exist: `100 ms` `is_suppressed` sleep finishes between `300 ms` poll ticks; incoming `Files` or `Image` updates `last_sent_content` while `cb.get_text()` at `:77` still returns prior OS text and re-sends it; failed `set_text`/`set_image` at `:148, :174` updates `last_sent_content` while leaving prior OS clipboard data to bounce back; OS text/image normalization alters bytes/hash) | CLIP-04 |
| L-CH-1: `RequestElevation` marks the session elevated with no prompt or auth (`elevation.rs:309-319` -> actual lines `elevation.rs:118-128`). | Confirmed | `crates/rferram-channels/src/elevation.rs:118-128` (`handle_elevation_request` increments `requests_received`, stores `true` into `session_elevated`, logs `info!`, and returns `Ok(())` without running `pkexec`, `runas`, or `launchctl`) | CH-01 |
| L-CH-2: passthrough only records frames, unbounded (`passthrough.rs:90-111`). | Confirmed | `crates/rferram-channels/src/passthrough.rs:29, 90-111` (`handle_device_data` hashes `data` and pushes a `DevicePassthroughFrame` onto `recorded_frames: Arc<Mutex<Vec<DevicePassthroughFrame>>>` with no cap and no OS peripheral forwarding) | CH-02 |

## Test suite verification (`cargo test -p rferram-channels --locked`)

Command executed: `cargo test -p rferram-channels --locked` -> exit code `0` (`11 passed; 0 failed; 0 ignored; 0 doc-tests`).

| Test name (`file:line`) | What it actually proves | What it does NOT prove |
| :--- | :--- | :--- |
| `clipboard::tests::test_clipboard_loopback_echo_suppression` (`clipboard.rs:256-264`) | Exercises `MockClipboardHandler` (`:205-253`), a test-only struct in `mod tests` using `is_suppressed.swap(false, SeqCst)`. | Never instantiates or calls `ClipboardChannelHandler`, `start_clipboard_monitor`, or `handle_remote_clipboard`. |
| `clipboard::tests::test_duplicate_image_hash_filtering` (`clipboard.rs:267-288`) | Exercises `calculate_image_hash` and `MockClipboardHandler::process_incoming_image`. | Never exercises `ClipboardChannelHandler`. |
| `elevation::tests::test_elevation_mechanism_mapping` (`elevation.rs:164-184`) | Static mapping from `PlatformDistribution` to `ElevationMechanism` and `command_program()`. | Any OS privilege check or command execution. |
| `elevation::tests::test_elevation_request_send_and_handle` (`elevation.rs:136-161`) | `request_host_elevation` sends `RequestElevation` to an `mpsc::channel` and `handle_elevation_request` sets `session_elevated = true`. | Real OS elevation (`pkexec`/`runas`/`launchctl`) or authorization. |
| `passthrough::tests::test_device_passthrough_send_and_handle` (`passthrough.rs:119-150`) | `send_device_data` emits `DevicePassthrough` on an `mpsc::channel` and `handle_device_data` pushes metadata into `recorded_frames`. | Real USB/smartcard redirection or memory bounding. |
| `passthrough::tests::test_device_passthrough_filtering_and_validation` (`passthrough.rs:153-190`) | Rejects whitespace-only `device_id` and filters/clears `recorded_frames`. | Real peripheral I/O. |
| `oob_ssh::tests::test_peer_ed25519_key_registration_and_verification` (`oob_ssh.rs:398-476`) | In-memory `register_peer_verifying_key`, `verify_peer_channel_bound_challenge`, and `revoke_peer_key` on a single `OobKeyIngestionManager` instance. | Reloading keys from `NistVault` across restarts, token expiration (`expires_at`), or daemon integration. |
| `oob_ssh::tests::test_openssh_key_parsing_and_russh_handler_auth` (`oob_ssh.rs:479-543`) | Direct Rust calls to `register_peer_openssh_key`, `Handler::auth_publickey_offered`, `Handler::auth_publickey`, and `build_russh_server_config()`. | Starting a `russh` TCP listener, SSH handshake/channel I/O, or `Handler::data` (`OOB_INGEST_ED25519:`). |
| `snapshot_transfer::tests::test_st_rust_age_snapshot_transfer_initiation` (`snapshot_transfer.rs:163-204`) | `initiate_snapshot_transfer` emits `ST_INIT:<id>:<path>` and `handle_incoming_st_bytes` updates `transferred_bytes` and `sha256_digest` in memory. | Actual file reading, disk writing, or end-to-end file transfer (audited in STEP 1F). |
| `tests::test_route_channel_message_all_variants` (`lib.rs:82-198`) | `VirtualChannelManager::route_channel_message` dispatches all 4 `VirtualChannelMessage` variants to the in-memory handlers. | Live QUIC transport, outbound clipboard monitoring, or OS side effects. |
| `tests::test_route_channel_message_postcard_wire_roundtrip` (`lib.rs:201-271`) | `encode_length_prefixed` / `decode_length_prefixed` round-trip for `WireMessage::Channel` variants. | End-to-end client-to-server channel behavior. |

## Role of `russh` across the rFerram workspace (input to R3 / R11)
- `russh` (`0.64.1`, `features = ["flate2", "aws-lc-rs"]` at `Cargo.toml:45`, `crates/rferram-channels/Cargo.toml:16`) is imported in only one file in the rFerram crates: `crates/rferram-channels/src/oob_ssh.rs:12-16` (it also appears in the unused vendored dependency `vendor/ST_Rust_Age/simply-transfer-core`).
- Neither `rferram-server`, `rferram-client`, `rferram-ui`, nor `rferram-net` starts `OobKeyIngestionManager` as a `russh` server, implements a `russh::client`, or calls `OobKeyIngestionManager::verify_peer_channel_bound_challenge`.
- Consequently, `russh` performs zero runtime work in rFerram today while pulling in a second cryptographic provider (`aws-lc-rs` alongside `ring`) and `RUSTSEC-2026-0495`; all SSH control-plane and pairing responsibilities belong in the shared `simply-transfer-link` transport crate (R11).

## Findings

### OOB-01: Embedded OOB SSH server (`OobKeyIngestionManager`) is dead code and disconnected from daemon authentication
- Severity: High
- Category: stub
- Requirement: R3
- Evidence: `crates/rferram-channels/src/lib.rs:38-45`
```rust
            clipboard_handler: ClipboardChannelHandler::new(),
            st_handler: SimplyTransferChannelHandler::new(),
            oob_ssh_manager: OobKeyIngestionManager::new(),
```
- Confidence: Read
- Impact: `OobKeyIngestionManager` is instantiated per session inside `VirtualChannelManager::new()`, never bound to a socket or port via `russh::server::Server`, has no corresponding SSH client in the workspace, and is never queried by `RFerramHostDaemon::handle_handshake` (`crates/rferram-server/src/daemon.rs:404-427`) when verifying `AuthMethod::Ed25519Signature`.
- Direction: Replace `crates/rferram-channels/src/oob_ssh.rs` with the shared `simply-transfer-link` authenticated control/pairing transport and wire the daemon's peer key store to a single daemon-wide trust registry.

### OOB-02: Any authenticated OOB SSH peer can register or overwrite keys for arbitrary `peer_id`s via `OOB_INGEST_ED25519`
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-channels/src/oob_ssh.rs:337-342`
```rust
        if let Ok(text) = std::str::from_utf8(data)
            && let Some(payload) = text.trim().strip_prefix(OOB_INGEST_FRAME_PREFIX)
            && let Some((peer_id, key_str)) = payload.split_once(':')
        {
            match self.register_peer_openssh_key(peer_id, key_str).await {
```
- Confidence: Read
- Impact: `auth_publickey` (`:298-318`) ignores the SSH `user` parameter and does not bind the connection to the authenticating key's `peer_id`, allowing any peer with a valid key in `authorized_peers` to send `OOB_INGEST_ED25519:<victim_peer_id>:<attacker_key>` and overwrite `<victim_peer_id>`'s public key in both `authorized_peers` and `NistVault` (`:150-156`).
- Direction: Bind each authenticated session to the specific `peer_id` whose key authenticated the session and restrict self-service key rotation strictly to that `peer_id`.

### OOB-03: `OobKeyIngestionManager::with_vault` silently falls back to a hardcoded, deterministic Ed25519 host key seed on vault error
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-channels/src/oob_ssh.rs:71-76`
```rust
            Err(_) => {
                let seed: [u8; 32] =
                    std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11));
                let keypair = Ed25519Keypair::from_seed(&seed);
                PrivateKey::from(keypair)
            }
```
- Confidence: Read
- Impact: If `vault.get_or_create_ed25519_identity("oob_ssh_host")` fails for any reason, `OobKeyIngestionManager` silently uses a publicly known static private key without even logging an error, allowing any attacker to compute the exact Ed25519 host keypair and impersonate the host.
- Direction: Propagate vault key-loading errors (`Result<Self>`) instead of falling back to a deterministic seed.

### OOB-04: `OobKeyIngestionManager` starts with an empty in-memory key registry, never reloads `NistVault` tokens, and ignores token expiration
- Severity: High
- Category: lifecycle
- Requirement: R3
- Evidence: `crates/rferram-channels/src/oob_ssh.rs:79-83`
```rust
        Self {
            authorized_peers: Arc::new(RwLock::new(HashMap::new())),
            vault: Arc::new(vault),
            host_key: Arc::new(host_key),
        }
```
- Confidence: Read
- Impact: Although `register_peer_verifying_key` writes `VaultTokenModel` entries under `oob_peer_{peer_id}` with a 30-day `expires_at` (`:138-152`), `with_vault` never reloads those entries on startup, `is_ssh_key_authorized` (`:226-239`) and `get_peer_key` (`:213-216`) never read `self.vault` or check `expires_at`, and because `authorized_peers` starts empty, no remote peer can ever authenticate over SSH (`:298-318`) to reach `Handler::data` (`:331-354`).
- Direction: Load persisted peer public keys from the vault on initialization, enforce expiration timestamps, and provide an explicit local/CLI pairing path to bootstrap initial trust.

### OOB-05: `russh` OOB server configuration speaks banner, KEX, and rapid auth-rejection replies to unauthenticated peers
- Severity: High
- Category: security
- Requirement: R8
- Evidence: `crates/rferram-channels/src/oob_ssh.rs:97-103`
```rust
        let config = Config {
            auth_rejection_time: Duration::from_millis(50),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
```
- Confidence: Read
- Impact: Against R8 ("OOB listener responds only to its authenticated app counterpart; all other traffic silently ignored"), any unauthenticated TCP scanner or non-app SSH client receives the `SSH-2.0-russh_0.64.1` banner, full Diffie-Hellman KEX with the server's Ed25519 host key, and immediate (`0 ms` initial, `50 ms` subsequent) `SSH_MSG_USERAUTH_FAILURE` responses advertising `publickey`, while `channel_open_session` (`:327`) accepts any post-auth `session` channel.
- Direction: Gate the OOB listener behind a pre-authenticated or single-packet authenticated framing mechanism (or VPN-scoped counterpart check) that silently drops unauthenticated probes before emitting protocol banners.

### CLIP-01: `ClipboardPayload::Files` is never polled/sent locally and incoming file lists are never written to the OS clipboard
- Severity: High
- Category: stub
- Requirement: R4
- Evidence: `crates/rferram-channels/src/clipboard.rs:177-184`
```rust
            ClipboardPayload::Files(files) => {
                info!(
                    count = files.len(),
                    "Injecting remote file path list into system clipboard"
                );
                let mut last = self.last_sent_content.lock().await;
                *last = ClipboardContent::Files(files);
            }
```
- Confidence: Read
- Impact: `start_clipboard_monitor` (`:75-120`) only checks `cb.get_text()` and `cb.get_image()`, never reading local clipboard file lists or sending `ClipboardPayload::Files`, and `handle_remote_clipboard` (`:177-184`) only records `ClipboardContent::Files(files)` in `last_sent_content` without touching the OS clipboard.
- Direction: Read local clipboard file lists via `arboard` (`Get::file_list`) in the monitor loop and integrate incoming file clipboard selections with the file-transfer channel and OS pasteboard.

### CLIP-02: `start_clipboard_monitor` is never invoked anywhere in the workspace and spawns an unstoppable 300 ms polling task that holds locks across `send().await`
- Severity: High
- Category: gap
- Requirement: R4
- Evidence: `crates/rferram-channels/src/clipboard.rs:74-90`
```rust
                let mut cb_lock = clipboard.lock().await;
                if let Some(cb) = cb_lock.as_mut() {
                    // 1. Check for Text Content
                    if let Ok(text) = cb.get_text()
```
- Confidence: Read
- Impact: Neither `rferram-server`, `rferram-client`, nor `rferram-ui` ever calls `ClipboardChannelHandler::start_clipboard_monitor` (`:56`), so outbound clipboard synchronization never runs in production; furthermore, if called, its `tokio::spawn` loop (`:64-122`) has no stop signal (violating R7) and holds both `clipboard.lock().await` (`:74`) and `last_sent.lock().await` (`:80, :98`) across `outgoing_tx.send(msg).await` (`:88, :115`), which deadlocks `handle_remote_clipboard` (`:136`) if `outgoing_tx` blocks.
- Direction: Wire `start_clipboard_monitor` into active client and server sessions with a session-scoped cancellation receiver, and drop both mutex guards before awaiting `outgoing_tx.send(msg)`.

### CLIP-03: `handle_remote_clipboard` silently discards `set_text`/`set_image` errors and does not validate image dimensions against buffer length
- Severity: Medium
- Category: gap
- Requirement: R4
- Evidence: `crates/rferram-channels/src/clipboard.rs:168-175`
```rust
                if let Some(cb) = cb_lock.as_mut() {
                    let img_data = ImageData {
                        width: width.max(1) as usize,
                        height: height.max(1) as usize,
                        bytes: std::borrow::Cow::Owned(data),
                    };
                    let _ = cb.set_image(img_data);
                }
```
- Confidence: Read
- Impact: `let _ = cb.set_text(text)` (`:148`) and `let _ = cb.set_image(img_data)` (`:174`) silently swallow OS clipboard failures while still updating `last_sent_content` and returning `Ok(())`, and `ClipboardPayload::Image` ignores `format` (`:154`) and never checks that `data.len() == width * height * 4` before passing peer-supplied dimensions to `arboard`.
- Direction: Validate `format == "raw/rgba8"` and `width * height * 4 == data.len()` prior to constructing `ImageData`, and update `last_sent_content` only after `set_text`/`set_image` succeeds (logging or propagating errors).

### CLIP-04: Clipboard echo suppression can bounce local or remote clipboard contents back across the wire
- Severity: Medium
- Category: gap
- Requirement: R4
- Evidence: `crates/rferram-channels/src/clipboard.rs:186-190`
```rust
        drop(cb_lock);

        tokio::time::sleep(Duration::from_millis(100)).await;
        self.is_suppressed.store(false, Ordering::SeqCst);
        Ok(())
```
- Confidence: Read
- Impact: Remote clipboard updates can bounce back as new local changes through four paths: (1) the `100 ms` `is_suppressed` window (`:188-189`) starts and finishes between two `300 ms` poll ticks (`:65`), leaving suppression solely to `last_sent_content` equality; (2) when `ClipboardPayload::Files` (`:177-184`) arrives, `last_sent_content` becomes `ClipboardContent::Files(...)` without modifying the OS clipboard, so on the next poll tick `cb.get_text()` (`:77`) or `cb.get_image()` (`:96`) sees `*last_lock != Text/ImageHash` and immediately broadcasts the pre-existing local clipboard back to the peer; (3) when `ClipboardPayload::Image` (`:151-176`) arrives (or `set_text`/`set_image` fails at `:148, :174`), `last_sent_content` changes to `ImageHash` while `cb.get_text()` (`:77`, which is checked unconditionally before `cb.get_image()` at `:96`) can still return an existing OS text target and bounce it back; and (4) OS line-ending (`\r\n`) or image RGBA/alpha normalization alters the read-back bytes so `cb.get_text()` / `calculate_image_hash` (`:194-199`) differs from what was received. Additionally, the unit tests (`:256-288`) only test a local `MockClipboardHandler` (`:205-253`), not `ClipboardChannelHandler`.
- Direction: Replace the `100 ms` `AtomicBool` sleep with monotonic suppression/sequence tracking, read back and canonicalize the actual OS clipboard state (both text and image hashes) while holding `cb_lock` after setting it, and test `ClipboardChannelHandler` directly.

### CH-01: `ElevationChannelHandler::handle_elevation_request` marks the session elevated with no OS privilege prompt or authentication
- Severity: High
- Category: security
- Requirement: R6
- Evidence: `crates/rferram-channels/src/elevation.rs:118-128`
```rust
    pub async fn handle_elevation_request(&self) -> Result<()> {
        let count = self.requests_received.fetch_add(1, Ordering::SeqCst) + 1;
        self.session_elevated.store(true, Ordering::SeqCst);
```
- Confidence: Read
- Impact: Any connected peer sending `VirtualChannelMessage::RequestElevation` immediately flips `session_elevated` to `true` (`:120`) without invoking `pkexec`, `runas`, `launchctl`, polkit, or UAC and without performing any authorization check, while nothing in `rferram-server` inspects `is_session_elevated()` or actually elevates daemon privileges.
- Direction: Gate elevation behind authenticated session permissions and invoke a real OS authorization mechanism (polkit / UAC / Authorization Services) with explicit status feedback to the client.

### CH-02: `DevicePassthroughHandler` is an in-memory telemetry stub that accumulates `DevicePassthroughFrame` entries without bound
- Severity: Medium
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-channels/src/passthrough.rs:103-110`
```rust
        let frame = DevicePassthroughFrame {
            device_id: trimmed_id.to_string(),
            payload_bytes: data.len(),
            sha256_digest,
            received_at: Instant::now(),
        };
        self.recorded_frames.lock().await.push(frame);
```
- Confidence: Read
- Impact: `handle_device_data` performs no USB, smartcard, PC/SC, or UHID device forwarding; instead, it pushes a `DevicePassthroughFrame` into `recorded_frames: Arc<Mutex<Vec<DevicePassthroughFrame>>>` on every inbound message and never calls `clear_recorded_frames()` outside tests (`:188`), allowing a peer to grow `recorded_frames` without bound.
- Direction: Either implement bounded ring-buffer queuing connected to a real OS smartcard/device redirection backend or remove `DevicePassthrough` from the active protocol surface until implemented.

### CH-03: `rferram-client` ignores `rferram-channels` and logs incoming `WireMessage::Channel` payloads at `INFO`, leaving virtual channels coupled to `rferram-ui`
- Severity: High
- Category: structure
- Requirement: R2
- Evidence: `crates/rferram-client/src/session.rs:283-285`
```rust
                WireMessage::Channel(vc) => {
                    info!(?vc, "Virtual channel payload received");
                }
```
- Confidence: Read
- Impact: `rferram-client/Cargo.toml:14` declares `rferram-channels`, yet `rferram-client` never instantiates `VirtualChannelManager`; `ClientSessionPipeline::demux_loop` (`:283-285`) logs the full `VirtualChannelMessage` (including plaintext clipboard contents) at `INFO` and drops it, while `VirtualChannelManager` is owned by `rferram-ui` (`crates/rferram-ui/src/session_controller.rs:161, 378`), preventing headless/CLI clients from using clipboard or file transfer channels (R2, R4, R5).
- Direction: Move client-side `VirtualChannelManager` ownership and stream routing out of `rferram-ui` into `rferram-client` and redact sensitive payload contents in log statements.

### CH-04: `rferram-channels` unit tests hardcode internal `/srv/devshare/` paths in clipboard file fixtures
- Severity: Low
- Category: sensitive-data
- Requirement: R11
- Evidence: `crates/rferram-channels/src/lib.rs:113-115`
```rust
            .route_channel_message(VirtualChannelMessage::Clipboard(ClipboardPayload::Files(
                vec!["/srv/devshare/report.pdf".to_string()],
            )))
```
- Confidence: Read
- Impact: Unit tests in `crates/rferram-channels/src/lib.rs:114, 120, 222-223` embed internal host workspace paths (`/srv/devshare/...`) in test fixtures instead of generic placeholder paths.
- Direction: Replace `/srv/devshare/...` fixture strings in `crates/rferram-channels/src/lib.rs` with neutral paths such as `/tmp/example.pdf`.

## Open questions for Claude
- Should `DevicePassthrough` (`passthrough.rs`) and `RequestElevation` (`elevation.rs`) be deferred/removed from `VirtualChannelMessage` for the initial production milestone so scope focuses on R4 (universal clipboard) and R5 (verified file transfer), or are OS elevation/passthrough in scope for Phase 2+?
- Once `simply-transfer-link` replaces `oob_ssh.rs` (R11), should `russh` be removed completely from `rferram-channels/Cargo.toml` so all SSH control/pairing lives exclusively in `simply-transfer-link`?
