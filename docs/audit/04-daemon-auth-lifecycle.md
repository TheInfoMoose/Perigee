# 04 Daemon Authentication and Session Lifecycle audit
Audited HEAD: 6e2cb2d

## Summary
- `RFerramHostDaemon::handle_handshake` (`crates/rferram-server/src/daemon.rs:379-459`) grants a full host session on a bare `HandshakeMessage::ClientHello` with zero credentials, accepts any non-empty byte slice for `AuthMethod::SystemUserPassword`, verifies `AuthMethod::Ed25519Signature` against the caller's self-supplied public key without a trusted-key allowlist or server-issued challenge, and accepts any non-empty `AuthMethod::VaultToken` during in-session re-authentication (`:735-736`).
- `handle_handshake` (`daemon.rs:373-376`) logs the entire `HandshakeMessage` via `fmt::Debug` at `INFO` level before redaction, writing raw passwords, `VaultToken` strings, and Ed25519 credential blobs to daemon logs, while `ClientSessionPipeline::demux_loop` (`crates/rferram-client/src/session.rs:187`) logs `AuthResponse::session_token` at `INFO` level.
- Stopped sessions are never removed from `RFerramHostDaemon::active_sessions` in production (`terminate_session` at `daemon.rs:605` is only called on an unreachable `run_capture_loop` setup error), and `rferram-server/src/main.rs` (`:40-64`) installs no `SIGINT`/`SIGTERM` signal handler and never calls `shutdown_all_sessions` (`daemon.rs:509`).
- Four of six `SessionState` variants in `rferram-client` (`Authenticating`, `NegotiatingTopology`, `Reconnecting`, `Terminated` at `crates/rferram-client/src/session.rs:66-70`) are never constructed anywhere in the workspace, and `serve_connection` never persists the `session_id` returned in `AuthResponse::session_token` (`daemon.rs:588`) into `NistVault` or resumes an existing `HostSessionEngine`.
- `HostSessionEngine::new` (`daemon.rs:81-92`) silently falls back to the Linux `WaylandPipeWire` stub when any platform capturer fails to initialize (including on Windows and macOS), `run_capture_loop` (`:157-173`) substitutes a synthetic 64 KiB `2560x1440` frame on capture errors, and `rferram-server` exposes no CLI flags (`main.rs:25-38`).

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Unauthenticated rejection | Stub | `crates/rferram-server/src/daemon.rs:379-399, 578-601`; any peer sending a non-empty `ClientHello` with `protocol_version != 0` immediately receives `AuthResponse { success: true }` and a provisioned `HostSessionEngine` | R3, R8 |
| Ed25519 auth against trusted keys | Partial | `crates/rferram-server/src/daemon.rs:352-369, 404-427, 712-734` and `crates/rferram-common/src/vault.rs:116-136`; verifies signature math against the client-supplied `credentials[0..32]` public key and client-chosen `credentials[128..]` challenge, with no trusted-key store check and no server cert comparison inside `handle_handshake` | R3 |
| Password auth | Stub | `crates/rferram-server/src/daemon.rs:453-458, 735`; `AuthMethod::SystemUserPassword` succeeds whenever `!credentials.is_empty()` | R3 |
| Token re-auth | Stub | `crates/rferram-server/src/daemon.rs:429-451, 707-750`; initial `VaultToken` auth checks `stored.session_token != presented_token` (`:444`) and `stored.expires_at == 0` (`:447`), while in-session `VaultToken` re-auth (`:736`) only checks `!credentials.is_empty()` and failed re-auth (`:738-749`) leaves the session active | R3, R7 |
| Credential logging | Partial | `crates/rferram-server/src/daemon.rs:373-376` logs `client_hello = ?handshake` at `INFO` level (exposing `AuthRequest::credentials`), and `crates/rferram-client/src/session.rs:187` logs `?h` at `INFO` level (exposing `AuthResponse::session_token`) | R3 |
| Session registry cleanup | Stub | `crates/rferram-server/src/daemon.rs:477-501, 604-606`; `provision_session` pushes to `active_sessions`, but `terminate_session` is never called when a session stops (`:254, 638, 766`), leaking every session in `active_sessions` | R7 |
| Client disconnect | Partial | `crates/rferram-server/src/daemon.rs:248-255, 638, 766`; server handles `ControlMessage::Disconnect` or uni-stream EOF by calling `session.stop()`, but client `QuicSessionBridge::stop` (`crates/rferram-client/src/bridge.rs:159-161`) and UI `disconnect_profile` (`crates/rferram-ui/src/session_controller.rs:805-818`) never send `ControlMessage::Disconnect` or close the QUIC connection | R7 |
| Server shutdown/signals | Missing | `crates/rferram-server/src/main.rs:40-64` and `crates/rferram-server/src/daemon.rs:509-514`; no `tokio::signal` handling exists in `main.rs` and `shutdown_all_sessions` is never called outside tests | R7 |
| Reconnect/resume | Missing | `crates/rferram-client/src/session.rs:69` (`SessionState::Reconnecting`) and `crates/rferram-server/src/daemon.rs:483` (`get_session`) are never used in production; `AuthResponse::session_token` (`:588`) is never persisted in `daemon.vault` | R7 |
| Multi-session | Partial | `crates/rferram-server/src/daemon.rs:283, 467-480, 1178-1239`; `active_sessions` holds multiple `Arc<HostSessionEngine>` instances concurrently, but each session spawns duplicate capture/encode/audio loops on display `1` with no concurrency limit or input arbitration | R6, R7 |
| Config surface (env vars / CLI) | Stub | `crates/rferram-server/src/main.rs:13-38`; reads only `RUST_LOG`, `RFERRAM_BIND_ADDR`, and `RFERRAM_SMOKE_TEST` from environment variables with zero CLI argument parsing | R2, R9 |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-AUTH-1: a bare `ClientHello` yields a session (`daemon.rs:379-399`). | Confirmed | `crates/rferram-server/src/daemon.rs:379-399, 578-601` (`handle_handshake` returns a new session UUID on `ClientHello`, and `serve_connection` sends `AuthResponse { success: true }` and provisions the session; `crates/rferram-client/src/session.rs:150-165` only ever sends `ClientHello`) | AUTH-01 |
| L-AUTH-2: Ed25519 auth checks key possession only; no trusted-key list (`daemon.rs:404-427`). | Confirmed | `crates/rferram-server/src/daemon.rs:352-369, 404-427, 712-734` and `crates/rferram-common/src/vault.rs:116-136` (verifies signature against the untrusted `verifying_key` in `credentials[0..32]` with no allowlist lookup, client-chosen `challenge`, and no cert check inside `handle_handshake`) | AUTH-02 |
| L-AUTH-3: `SystemUserPassword` accepts any non-empty bytes (`:453-458, :735`); `VaultToken` re-auth checks non-empty only (`:736`). | Confirmed | `crates/rferram-server/src/daemon.rs:453-458, 735-749` (`SystemUserPassword` checks only `!credentials.is_empty()` in both places; in-session `VaultToken` at `:736` checks only `!credentials.is_empty()` and does not stop the session on failure) | AUTH-03 |
| L-AUTH-4: the full handshake, including credentials, is logged at `info` (`daemon.rs:373`). | Confirmed | `crates/rferram-server/src/daemon.rs:373-376` (`info!(client_hello = ?handshake, ...)` logs unredacted `AuthRequest::credentials`; `crates/rferram-client/src/session.rs:187` logs `AuthResponse::session_token` at `info`) | AUTH-04 |
| L-LIFE-1: `terminate_session` is never called from `serve_connection`, so stopped sessions stay in `active_sessions`. | Confirmed | `crates/rferram-server/src/daemon.rs:604-606, 613-832` (`terminate_session` is only called if `run_capture_loop` returns `Err`, which is unreachable because `run_capture_loop` at `:207` and `start_loopback_capture` at `audio.rs:93` always return `Ok(())`) | LIFE-01 |
| L-LIFE-2: `main` has no signal handling; `shutdown_all_sessions` (`:509`) is never called. | Confirmed | `crates/rferram-server/src/main.rs:40-64` and `crates/rferram-server/src/daemon.rs:509-514` (`shutdown_all_sessions` is only called in tests at `daemon.rs:1236` and `lib.rs:407`) | LIFE-02 |
| L-LIFE-3: `SessionState::Reconnecting` (`client session.rs:69`) is never used; no resume path exists. | Confirmed | `crates/rferram-client/src/session.rs:64-71, 96, 188-190` and `crates/rferram-server/src/daemon.rs:483, 586-601` (`Authenticating`, `NegotiatingTopology`, `Reconnecting`, and `Terminated` are never constructed; server never stores `AuthResponse::session_token` or resumes via `get_session`) | LIFE-03 |
| L-LIFE-4: `send_auth_failure_and_retain` spawns a delayed task (`:516-528`); check what it holds alive. | Confirmed | `crates/rferram-server/src/daemon.rs:516-531` (spawns an untracked task that sleeps `50 ms` holding `conn: Arc<QuicConnectionManager>`—which retains `quinn::Connection`, `Arc<NetworkMetrics>`, and `QuicTransportConfig`—without calling `connection.close(...)`) | LIFE-04 |
| L-LIFE-5: capture backend failure silently falls back to the Wayland stub and capture errors to a fake 2560x1440 frame (`:82-92, :162-172`). | Confirmed | `crates/rferram-server/src/daemon.rs:81-92, 155-175` (and `crates/rferram-server/src/lib.rs:106-108` `.ok()` on `LinuxInputController::new`) | LIFE-05 |
| L-LIFE-6: server config is env vars only (`main.rs:25-36`); no CLI arguments. | Confirmed | `crates/rferram-server/src/main.rs:12-38` (reads only `RUST_LOG`, `RFERRAM_BIND_ADDR`, and `RFERRAM_SMOKE_TEST`; no CLI parser or flags) | LIFE-06 |

## Session state machine

### Server session states (`RFerramHostDaemon` / `HostSessionEngine`)
`rferram-server` does not define an explicit session state enum; state is split across `RFerramHostDaemon::active_sessions` (`Vec<Arc<HostSessionEngine>>` at `crates/rferram-server/src/daemon.rs:283`), `HostSessionEngine::is_active` (`AtomicBool` at `:46`), `HostSessionEngine::stop_tx` (`watch::Sender<bool>` at `:47`), and `HostAudioCapturer::is_running` (`AtomicBool` at `crates/rferram-server/src/audio.rs:33`).

| State | Representation | Entry trigger & `file:line` | Outgoing transitions & `file:line` |
| :--- | :--- | :--- | :--- |
| **1. Accepted (Unauthenticated)** | `QuicConnectionManager` passed to `serve_connection`; not in `active_sessions` | `listener.accept().await` yields connection (`crates/rferram-server/src/main.rs:40`, `crates/rferram-net/src/endpoint.rs:174-186`) -> `tokio::spawn` calls `daemon.serve_connection(conn)` (`main.rs:43-44`), which awaits `conn.recv_uni_message().await` (`crates/rferram-server/src/daemon.rs:547-550`) | -> **Rejected** on stream read error (`:547-550`), non-`Handshake` wire message (`:552-556`), Ed25519 cert digest mismatch (`:565-576`), or `handle_handshake` error (`:578-584`).<br>-> **Active** when `handle_handshake` returns `Ok(session_id)` (`:578-604`). |
| **2. Rejected (Retained 50 ms)** | No `HostSessionEngine`; `Arc<QuicConnectionManager>` held by detached sleep task | Cert mismatch (`daemon.rs:574`) or `handle_handshake` error (`:581`) calls `send_auth_failure_and_retain` (`:516-531`), sending `AuthResponse { success: false, .. }` and sleeping `50 ms` | -> **Dropped** after `tokio::time::sleep(Duration::from_millis(50)).await` (`:528-529`) drops `conn` (no `connection.close` call). |
| **3. Active** | In `active_sessions`; `is_active == true`, `*stop_rx == false`, `audio_capturer.is_running == true` | `serve_connection` sends `AuthResponse { success: true, session_token: Some(session_id), .. }` (`daemon.rs:586-593`), calls `provision_session` (`:467-480, 599-601`), calls `session.run_capture_loop(outgoing_tx)` (`:130-208, 604`), and spawns QUIC Tasks 1–4 (`:613, 649, 691, 778`) | -> **Active (unchanged)** on in-session `HandshakeMessage::AuthRequest` in Task 3 (`:707-750`), even when `valid == false` (`:738-749`).<br>-> **Stopped (Leaked in Registry)** via `session.stop()` (`:269-274`).<br>-> **Terminated** via `terminate_session` (`:492-501`) or `shutdown_all_sessions` (`:509-514`). |
| **4. Stopped (Leaked in Registry)** | Still in `active_sessions`; `is_active == false`, `*stop_rx == true`, `audio_capturer.is_running == false` | `HostSessionEngine::stop(&self)` (`daemon.rs:269-274`) invoked by: (a) `ControlMessage::Disconnect` (`:248-255`), (b) Task 1 outbound send error (`:632-639`), or (c) Task 3 inbound uni-stream error (`:760-767`) | **None in production** (`terminate_session` and `shutdown_all_sessions` are never called after `run_capture_loop` starts). |
| **5. Terminated (Removed)** | Removed from `active_sessions`; `is_active == false`, `*stop_rx == true`, `audio_capturer.is_running == false` | `RFerramHostDaemon::terminate_session` (`daemon.rs:492-501`) or `shutdown_all_sessions` (`:509-514`) | Terminal state (reachable only in unit tests `daemon.rs:1105, 1222, 1236`, `lib.rs:400, 407`, or dead `run_capture_loop` error branch at `daemon.rs:604-606`). |

**Missing server states and transitions:**
- **No challenge-response handshake state (`ClientHello -> ServerHello -> AuthRequest -> AuthResponse`)**: `ClientHello` transitions directly from **Accepted** to **Active** (`daemon.rs:379-399, 578-601`) without issuing `ServerHello` or requiring `AuthRequest`.
- **No transition out of Active on failed in-session `AuthRequest`**: When Task 3 evaluates `valid == false` (`daemon.rs:738-749`), it replies with `AuthResponse { success: false, .. }` but neither calls `session_uni.stop()` nor removes the session.
- **No transition from Stopped to Terminated in production**: `session.stop()` (`daemon.rs:269-274`) has no back-reference to `RFerramHostDaemon::active_sessions`, leaving stopped sessions in memory permanently.
- **No `Suspended` / `Resuming` transition**: `AuthResponse::session_token` (`daemon.rs:588`) is a random UUID that is never stored in `daemon.vault` and never matched against `active_sessions` via `get_session` (`:483`).
- **Incomplete error transitions to Stopped**: Task 2 (`daemon.rs:674-681`, datagram loop), Task 4 (`:784-786`, bidi stream loop), the video capture loop (`:182-185`), and the audio capture loop (`audio.rs:85-88`) `break` on channel/transport errors without calling `session.stop()`.

### Client session states (`SessionState` in `crates/rferram-client/src/session.rs:64-71`)
`ClientSessionPipeline` exposes `watch::Receiver<SessionState>` (`crates/rferram-client/src/session.rs:64-71, 76, 92`), while `rferram-ui` maintains a separate string status (`crates/rferram-ui/src/session_controller.rs:528-532, 613, 628, 677-679, 742, 814`).

| `SessionState` variant | Defined at | Entry trigger & `file:line` | Outgoing transitions & `file:line` | Status |
| :--- | :--- | :--- | :--- | :--- |
| `Connecting` | `session.rs:65` | Initial state in `ClientSessionPipeline::spawn` (`crates/rferram-client/src/session.rs:96`); Task 3 sends `HandshakeMessage::ClientHello` (`:145-165`) | -> `Active` when `demux_loop` receives `HandshakeMessage::AuthResponse { success: true, .. }` (`session.rs:188-190`) | **Used** |
| `Authenticating` | `session.rs:66` | **None** (never constructed anywhere in workspace) | **None** | **Dead / Missing** |
| `NegotiatingTopology` | `session.rs:67` | **None** (never constructed anywhere in workspace; `WireMessage::Topology` at `session.rs:192-194` only logs `info!`) | **None** | **Dead / Missing** |
| `Active` | `session.rs:68` | `demux_loop` matches `HandshakeMessage::AuthResponse { success: true, .. }` and calls `state_tx.send(SessionState::Active)` (`session.rs:188-190`) | **None** (never transitions out of `Active`, even on channel close, `Disconnect`, or `QuicSessionBridge::stop`) | **Used** |
| `Reconnecting { attempt, reason }` | `session.rs:69` | **None** (never constructed anywhere in workspace) | **None** | **Dead / Missing** |
| `Terminated { reason }` | `session.rs:70` | **None** (never constructed anywhere in workspace) | **None** | **Dead / Missing** |

**Missing client transitions:**
- `Connecting -> Authenticating` (on `ServerHello` / challenge receipt) and `Connecting | Authenticating -> Terminated` (on `AuthResponse { success: false, .. }`, which `session.rs:188` silently ignores).
- `Active -> NegotiatingTopology -> Active` (on `WireMessage::Topology`, which `session.rs:192-194` only logs).
- `Active -> Reconnecting { attempt, reason }` (on QUIC stream/datagram error in `crates/rferram-client/src/bridge.rs:89-92, 109-112, 129-134`).
- `Connecting | Active | Reconnecting -> Terminated { reason }` (on `ControlMessage::Disconnect` at `session.rs:281`, `wire_rx` EOF at `session.rs:184, 288`, or `QuicSessionBridge::stop()` at `bridge.rs:159-161`).

## Spawned tasks

Every `tokio::spawn` in the server call graph from `main.rs` into `RFerramHostDaemon::serve_connection` (`crates/rferram-server/src/daemon.rs:541-832`) and its callees:

| # | Task | Spawn location (`file:line`) | Cloned state held | What ends the task | Handle awaited or aborted? |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 0 | **Per-connection accept wrapper** (calls `serve_connection`) | `crates/rferram-server/src/main.rs:43-60` | `daemon_clone: Arc<RFerramHostDaemon>`, `conn: QuicConnectionManager` | Exits as soon as `serve_connection(conn).await` returns (`Ok(session)` right after spawning Tasks 2–7, or `Err(e)` on handshake error). | **No** (`JoinHandle` dropped at `main.rs:43`). |
| 1 | **Auth-failure connection retainer** (`send_auth_failure_and_retain`) | `crates/rferram-server/src/daemon.rs:527-530` (called from `:574, 581`) | `conn: Arc<QuicConnectionManager>` | `tokio::time::sleep(Duration::from_millis(50)).await` completes (`:528`), then drops `conn` (`:529`). | **No** (`JoinHandle` dropped at `daemon.rs:527`). |
| 2 | **Video frame capture & encode loop** (`HostSessionEngine::run_capture_loop`) | `crates/rferram-server/src/daemon.rs:146-201` (called from `:604`) | `is_active: Arc<AtomicBool>`, `stop_rx: watch::Receiver<bool>`, `capturer: Arc<Mutex<Box<dyn FrameCapturer>>>`, `encoder: Arc<Mutex<Box<dyn VideoEncoder>>>`, `force_keyframe: Arc<AtomicBool>`, `video_tx: mpsc::Sender<WireMessage>` | `!is_active.load(Relaxed)` or `*stop_rx.borrow()` at loop head (`:149`), `video_tx.send(...).await.is_err()` (`:182-185`), or `stop_rx.changed()` during inter-frame sleep (`:195-198`). | **No** (`JoinHandle` dropped at `daemon.rs:146`). |
| 3 | **Audio 20 ms loopback capture loop** (`HostAudioCapturer::start_loopback_capture`) | `crates/rferram-server/src/audio.rs:69-91` (called from `daemon.rs:203-205` -> `:604`) | `is_running: Arc<AtomicBool>`, `backend: CaptureBackendType`, `tx: mpsc::Sender<WireMessage>` | `!is_running.load(SeqCst)` before/after 20 ms sleep (`audio.rs:76, 78-80`, set by `audio_capturer.stop()` at `:98`), or `tx.send(...).await.is_err()` (`:85-88`). | **No** (`JoinHandle` dropped at `audio.rs:69`). |
| 4 | **Task 1: Outbound QUIC media/control pump** (`serve_connection`) | `crates/rferram-server/src/daemon.rs:613-642` | `conn_outbound: Arc<QuicConnectionManager>`, `session_outbound: Arc<HostSessionEngine>`, `stop_outbound: watch::Receiver<bool>`, `outgoing_rx: mpsc::Receiver<WireMessage>` | `!session_outbound.is_running()`, `*stop_outbound.borrow()`, `stop_outbound.changed()` during `outgoing_rx.recv()` (`:614-624`), `outgoing_rx` closed, or `send_datagram` / `send_uni_message` error (`:632-639`, which calls `session_outbound.stop()`). Does **not** select on `stop_outbound` while awaiting `send_uni_message` (`:630`). | **No** (`JoinHandle` dropped at `daemon.rs:613`). |
| 5 | **Task 2: Inbound QUIC datagram router** (`serve_connection`) | `crates/rferram-server/src/daemon.rs:649-684` | `conn_dgram: Arc<QuicConnectionManager>`, `session_dgram: Arc<HostSessionEngine>`, `channel_mgr_dgram: Arc<VirtualChannelManager>`, `stop_dgram: watch::Receiver<bool>` | `!session_dgram.is_running()`, `*stop_dgram.borrow()`, `stop_dgram.changed()` during `recv_datagram()` (`:650-654`), or `conn_dgram.recv_datagram()` returning `Err` (`:674-681`, including a single malformed datagram decode error; does **not** call `session_dgram.stop()`). | **No** (`JoinHandle` dropped at `daemon.rs:649`). |
| 6 | **Task 3: Inbound QUIC unidirectional stream router** (`serve_connection`) | `crates/rferram-server/src/daemon.rs:691-771` | `conn_uni: Arc<QuicConnectionManager>`, `session_uni: Arc<HostSessionEngine>`, `channel_mgr_uni: Arc<VirtualChannelManager>`, `stop_uni: watch::Receiver<bool>` | `!session_uni.is_running()`, `*stop_uni.borrow()`, `stop_uni.changed()` during `recv_uni_message()` (`:692-696`), or `conn_uni.recv_uni_message()` returning `Err` (`:760-767`, which calls `session_uni.stop()`). | **No** (`JoinHandle` dropped at `daemon.rs:691`). |
| 7 | **Task 4: Inbound QUIC bidirectional stream router** (`serve_connection`) | `crates/rferram-server/src/daemon.rs:778-829` | `conn_bi: Arc<QuicConnectionManager>`, `session_bi: Arc<HostSessionEngine>`, `channel_mgr_bi: Arc<VirtualChannelManager>`, `stop_bi: watch::Receiver<bool>` | `!session_bi.is_running()`, `*stop_bi.borrow()`, `stop_bi.changed()` during `accept_bi()` (`:779-783`), or `accept_bi()` returning `Err` (`:784-786`; does **not** call `session_bi.stop()`). Does **not** select on `stop_bi` during `recv_stream.read_to_end(...)` (`:787-792`). | **No** (`JoinHandle` dropped at `daemon.rs:778`). |

## Findings

### AUTH-01: Bare `HandshakeMessage::ClientHello` grants an authenticated session without credentials
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-server/src/daemon.rs:379-399, 578-601`
```rust
                    bail!("ClientHello must advertise at least one AuthMethod");
                }
                Ok(uuid::Uuid::new_v4().to_string())
```
- Confidence: Read
- Impact: Any remote peer that sends a `HandshakeMessage::ClientHello` with `protocol_version != 0` and non-empty `client_id`, `supported_codecs`, and `auth_methods` immediately receives `AuthResponse { success: true, session_token: Some(session_id), .. }` (`:586-593`) and a live `HostSessionEngine` with screen/audio streaming, OS input injection, clipboard access, file transfer, and privilege elevation; furthermore, the live client (`crates/rferram-client/src/session.rs:150-165`) only ever sends `ClientHello` and never sends `AuthRequest`.
- Direction: Require a multi-step handshake where `ClientHello` only negotiates parameters and responds with a server-generated `ServerHello` challenge, provisioning a session only after a verified `AuthRequest` completes.

### AUTH-02: `AuthMethod::Ed25519Signature` verifies key possession only, lacks a trusted-key allowlist or server nonce, and omits cert comparison in `handle_handshake`
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-server/src/daemon.rs:412-427`
```rust
                    self.verify_handshake_challenge(
                        &verifying_key,
                        challenge,
```
- Confidence: Read
- Impact: `AuthMethod::Ed25519Signature` extracts `verifying_key` (`credentials[0..32]`) and `challenge` (`credentials[128..]`) directly from the peer's payload and verifies only that the peer signed its own chosen `challenge` with its own untrusted `verifying_key`, without checking `NistVault` or `OobKeyIngestionManager::authorized_keys`, while `handle_handshake` (`:404-427`) never compares `credentials[32..64]` against the server's TLS certificate hash (`serve_connection` only checks `conn.peer_cert_sha256()` at `:570-571` when `Some`, and the unit test in `crates/rferram-server/src/lib.rs:274-285` only fails because it passes a signature computed over `[0x5A; 32]` alongside `[0xFF; 32]`).
- Direction: Issue a fresh 32-byte server challenge nonce per handshake, verify `verifying_key` against a persistent pinned peer allowlist, and enforce exact server `tls_cert_sha256` matching inside `handle_handshake`.

### AUTH-03: `SystemUserPassword` and in-session `VaultToken` re-auth accept any non-empty bytes, and initial `VaultToken` auth ignores clock expiry
- Severity: Critical
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-server/src/daemon.rs:453-458, 735-737`
```rust
                            AuthMethod::SystemUserPassword => !credentials.is_empty(),
                            AuthMethod::VaultToken => !credentials.is_empty(),
                        };
```
- Confidence: Read
- Impact: Any 1-byte payload authenticates as `AuthMethod::SystemUserPassword` during initial handshake (`:453-458`) and as either `SystemUserPassword` or `VaultToken` during in-session re-auth (`:735-736`), while initial `VaultToken` auth (`:444-449`) uses non-constant-time `!=` comparison and only checks `stored.expires_at == 0` instead of current UNIX time, and failed in-session re-auth (`:738-749`) leaves the session running.
- Direction: Remove `SystemUserPassword` or wire it to OS authentication, validate `VaultToken` against `NistVault` using constant-time comparison and current UNIX timestamp checks in all paths, and terminate the session on re-authentication failure.

### AUTH-04: Handshake credentials and session tokens are logged in cleartext at `INFO` level
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-server/src/daemon.rs:373-376` and `crates/rferram-client/src/session.rs:187`
```rust
        info!(
            client_hello = ?handshake,
            "Processing incoming client connection handshake"
```
- Confidence: Read
- Impact: Because `HandshakeMessage` derives unredacted `fmt::Debug` (`crates/rferram-protocol/src/lib.rs:160`) and `rferram-server` defaults to `EnvFilter::new("info")` (`crates/rferram-server/src/main.rs:15`), `daemon.rs:373-376` logs raw `AuthRequest::credentials` (passwords, vault tokens, and signatures) to stdout/journald on every connection, and `session.rs:187` logs `AuthResponse::session_token` at `INFO` level on the client.
- Direction: Redact `HandshakeMessage::AuthRequest::credentials` and `HandshakeMessage::AuthResponse::session_token` in a custom `fmt::Debug` implementation and log only non-sensitive metadata at `INFO` level.

### AUTH-05: Unauthenticated connections have no handshake timeout, pre-auth frame size cap below 16 MiB, or session concurrency limit
- Severity: High
- Category: security
- Requirement: R7, R8
- Evidence: `crates/rferram-server/src/main.rs:40-45` and `crates/rferram-server/src/daemon.rs:547-550`
```rust
        let initial_wire = conn
            .recv_uni_message()
            .await
```
- Confidence: Read
- Impact: Before authenticating, any peer that completes a QUIC handshake can hold `serve_connection` open indefinitely via QUIC keep-alives (`daemon.rs:547`), force a `16 MiB + 4` allocation in `recv_uni_message` (`crates/rferram-net/src/manager.rs:108-110`), or flood `ClientHello` messages to spawn unbounded concurrent 60 fps capture/encode and 50 Hz audio tasks (`main.rs:40-61`).
- Direction: Enforce a short handshake deadline (e.g. `2 s`), restrict pre-auth stream reads to a small handshake frame cap (e.g. `4 KiB`), and cap concurrent handshakes and active sessions.

### LIFE-01: `terminate_session` is never called when a session stops, leaking `HostSessionEngine` instances in `active_sessions`
- Severity: High
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-server/src/daemon.rs:477-479, 604-606, 638, 766`
```rust
        if let Err(e) = session.run_capture_loop(outgoing_tx).await {
            self.terminate_session(&session_id).await;
            return Err(e);
```
- Confidence: Read
- Impact: `serve_connection` only calls `self.terminate_session(&session_id)` inside the unreachable `Err` branch of `run_capture_loop` (`:604-606`); when a client disconnects via `ControlMessage::Disconnect` (`:254`) or QUIC stream closure (`:638, 766`), only `session.stop()` is called, leaving the stopped `Arc<HostSessionEngine>` (and its capturer, encoder, input injector, and channel buffers) in `RFerramHostDaemon::active_sessions` for the lifetime of the daemon.
- Direction: Remove the session from `RFerramHostDaemon::active_sessions` whenever `stop_rx` triggers or any session transport pump terminates.

### LIFE-02: `rferram-server` has no signal handling and never invokes `shutdown_all_sessions`
- Severity: High
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-server/src/main.rs:40-64` and `crates/rferram-server/src/daemon.rs:509-514`
```rust
    pub async fn shutdown_all_sessions(&self) {
        let mut sessions = self.active_sessions.lock().await;
        for session in sessions.drain(..) {
```
- Confidence: Read
- Impact: `main.rs` has no `SIGINT` (`ctrl_c`) or `SIGTERM` handler and never calls `daemon.shutdown_all_sessions()` or closes the `QuicServerListener` endpoint when exiting (`main.rs:63`), leaving active sessions, virtual input devices, and QUIC connections without graceful teardown.
- Direction: Select on `tokio::signal::ctrl_c()` / `SIGTERM` in `main.rs`, invoke `daemon.shutdown_all_sessions().await`, and close the QUIC listener before process exit.

### LIFE-03: Client `SessionState` lifecycle variants are dead and no session-resume path exists
- Severity: High
- Category: gap
- Requirement: R7
- Evidence: `crates/rferram-client/src/session.rs:64-71, 188-190`
```rust
    Reconnecting { attempt: u32, reason: String },
    Terminated { reason: String },
}
```
- Confidence: Read
- Impact: `SessionState::{Authenticating, NegotiatingTopology, Reconnecting, Terminated}` are never constructed anywhere in the workspace, `demux_loop` ignores `AuthResponse { success: false }` (`:188`) and `ControlMessage::Disconnect` (`:281`), and `serve_connection` never stores `AuthResponse::session_token` (`daemon.rs:588`) into `NistVault` or looks up prior sessions via `get_session` (`daemon.rs:483`), making seamless session resume impossible.
- Direction: Wire all `SessionState` transitions across `ClientSessionPipeline` and `QuicSessionBridge`, persist issued session tokens with expiration timestamps, and implement session lookup/reattachment on reconnect.

### LIFE-04: `send_auth_failure_and_retain` spawns an untracked delayed task holding `Arc<QuicConnectionManager>` without closing the connection
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-server/src/daemon.rs:527-530`
```rust
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(conn);
```
- Confidence: Read
- Impact: On every rejected handshake (`:574, 581`), `send_auth_failure_and_retain` spawns a detached task holding `conn: Arc<QuicConnectionManager>` (`quinn::Connection`, `Arc<NetworkMetrics>`, `QuicTransportConfig`) for 50 ms after `send_uni_message` finishes and then drops `conn` without calling `conn.connection.close(...)`, relying on `Drop` after an arbitrary sleep.
- Direction: Send an explicit QUIC `APPLICATION_CLOSE` frame after flushing `AuthResponse` (or reject unauthenticated peers before opening application streams) instead of spawning an untracked sleep task.

### LIFE-05: Capture backend initialization silently falls back to the Linux Wayland stub and capture errors emit synthetic `2560x1440` frames
- Severity: High
- Category: stub
- Requirement: R6, R12
- Evidence: `crates/rferram-server/src/daemon.rs:81-92, 155-175`
```rust
        let capturer =
            Self::build_frame_capturer(backend_type, 1, width, height).unwrap_or_else(|_| {
                Box::new(
```
- Confidence: Read
- Impact: When `build_frame_capturer` fails on any OS, `HostSessionEngine::new` silently constructs `LinuxCaptureController::new(CaptureBackendType::WaylandPipeWire, 1, width, height)`, `run_capture_loop` (`:157-173`) replaces any `capture_next_frame()` error with a synthetic 64 KiB gray `2560x1440` frame (`vec![128u8; 64 * 1024]`), and `NativeInputInjector::new` (`crates/rferram-server/src/lib.rs:107`) swallows `LinuxInputController::new` errors via `.ok()`, hiding broken capture and input backends.
- Direction: Propagate `build_frame_capturer`, `capture_next_frame`, and `NativeInputInjector` errors to the caller so failed sessions report an error or terminate cleanly instead of streaming synthetic stub data.

### LIFE-06: `rferram-server` has no CLI interface and is configured only through 3 environment variables
- Severity: High
- Category: gap
- Requirement: R2, R9
- Evidence: `crates/rferram-server/src/main.rs:25-38`
```rust
    let bind_addr: SocketAddr = std::env::var("RFERRAM_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4433".to_string())
        .parse()?;
```
- Confidence: Read
- Impact: `rferram-server` accepts no CLI arguments (`--help`, `--version`, `--bind`, `--interface`, `--backend`, key enrollment, or daemon lifecycle subcommands), hardcodes `host_name = "rferram-host-node"` (`main.rs:21`), and cannot be operated or configured as a CLI-first standalone daemon (`R2`, `R9`).
- Direction: Add a CLI interface to `rferram-server` supporting bind address/interface selection, display mode selection, peer key management, and service lifecycle commands.

### LIFE-07: Session I/O tasks have asymmetric teardown, uncancelled stream reads/writes, and single-packet datagram loop termination
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-server/src/daemon.rs:674-681, 787-792`
```rust
                let Ok(data) = recv_stream
                    .read_to_end(rferram_protocol::MAX_WIRE_FRAME_BYTES + 4)
                    .await
```
- Confidence: Read
- Impact: Task 4 awaits `recv_stream.read_to_end(...)` (`:787-792`) and Task 1 awaits `conn_outbound.send_uni_message(...)` (`:630`) outside `tokio::select!` with `stop_rx.changed()`, allowing a stalled stream to keep the task and `Arc<QuicConnectionManager>` alive after `session.stop()`; meanwhile, Task 2 (`:674-681`) exits permanently on a single malformed datagram (`recv_datagram` conflates transport and decode errors in `crates/rferram-net/src/manager.rs:68-81`), and neither Task 2 (`:680`) nor Task 4 (`:785`) calls `session.stop()` when the QUIC connection closes.
- Direction: Wrap all per-stream reads and writes in `tokio::select!` against `stop_rx.changed()`, separate datagram decode errors from QUIC connection closure in `recv_datagram`, and trigger unified session teardown from any fatal transport error.

### LIFE-08: Peer-supplied `BandwidthEstimate::bytes_per_sec` can overflow `u64` in debug builds or truncate to `0` kbps
- Severity: Medium
- Category: security
- Requirement: R3, R7
- Evidence: `crates/rferram-server/src/daemon.rs:235-246`
```rust
                    ..
                } => {
                    let bitrate_kbps = (bytes_per_sec * 8 / 1000) as u32;
```
- Confidence: Read
- Impact: Computing `bytes_per_sec * 8` on an unvalidated peer-controlled `u64` panics with integer overflow in debug builds when `bytes_per_sec > u64::MAX / 8`, and in release builds wraps and truncates via `as u32` without clamping to valid encoder bitrate bounds.
- Direction: Use `bytes_per_sec.saturating_mul(8) / 1000` and clamp the resulting bitrate to a sane `[MIN_BITRATE_KBPS, MAX_BITRATE_KBPS]` range before updating the encoder.

## Open questions for Claude
- Should `HandshakeMessage::ClientHello` be removed in favor of bootstrapping every `rferram-server` session through `simply-transfer-link` (`st-control/1` over SSH issuing a one-time QUIC session ticket), or should `rferram-protocol` retain an in-band challenge-response handshake (`ClientHello -> ServerHello(nonce) -> AuthRequest -> AuthResponse`) for standalone QUIC sessions?
- When multiple viewers connect concurrently (`RFerramHostDaemon::active_sessions`), should they share a single capture/encode broadcast pipeline with explicit input-control arbitration, or should `rferram-server` enforce a single active session per display?
