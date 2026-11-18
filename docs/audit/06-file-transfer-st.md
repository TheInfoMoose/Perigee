# 06 File Transfer and ST_Rust_Age audit
Audited HEAD: 6f23b83

## Summary
- `SimplyTransferChannelHandler` (`crates/rferram-channels/src/snapshot_transfer.rs:32-150`) is a metadata-only stub: `initiate_snapshot_transfer` (`:55-112`, never called outside unit tests) hardcodes `total_bytes: 1024 * 1024 * 50` (`:96`), immediately destroys the OS snapshot at `:75` before any file is opened, and sends only a single `ST_INIT:{transfer_id}:{source_path}` header (`:105-109`) without ever reading or transmitting file bytes.
- On the receiving side, `handle_incoming_st_bytes` (`snapshot_transfer.rs:115-149`) never creates a `SnapshotTransferSession` when `ST_INIT:` arrives from a remote peer (`:120-125`), never opens or writes `target_path` on disk, credits any subsequent raw bytes to `active_transfers.last_mut()` (`:136`), and overwrites `sha256_digest` with each individual chunk's hash (`:116, 139`) rather than verifying whole-file integrity.
- A remote `ST_INIT:` payload does not reach `simply-transfer-snapshots` because `handle_incoming_st_bytes` (`:117-132`) only searches `active_transfers`; however, when `initiate_snapshot_transfer` runs locally on Linux, `LinuxSnapshotDriver` (`vendor/ST_Rust_Age/simply-transfer-snapshots/src/linux.rs:39-68, 112-115`) executes `lvcreate -s -n <snap_id> -l 100%FREE` and shells out via `sh -c` with `/dev/mapper/*-...` wildcards for `mount` and `lvremove -f`, while macOS `ApfsSnapshotDriver` (`macos.rs:51-65`) is a broken `"id-placeholder"` stub and Windows `VssSnapshotDriver` (`windows.rs:41-48, 91-95`) interpolates unescaped strings into PowerShell.
- `vendor/ST_Rust_Age` in rFerram has received zero upstream code syncs since initial scaffolding (`789f472`, 2026-10-25), leaving `simply-transfer-core`, `simply-transfer-crypto`, and `simply-transfer-snapshots` without upstream fixes `STEP 1B` and `2A–2G` (`russh_server.rs` still contains arbitrary `sh -c` / `cmd /c` execution at `:630-644`, unauthenticated `~/.ssh/authorized_keys` injection at `:748-789`, and unconfined file writes at `:385-405`, though `simply-transfer-core` is completely unused by rFerram).
- Upstream `ST_Rust_Age` Phase 4 (`simply-transfer-link`: SSH `st-control/1` subsystem, pinned host keys, one-time-secret pairing, `SharePolicy`, and ticketed QUIC streams on `aws-lc-rs`) directly replaces `oob_ssh.rs`, `PeerCertChannelVerifier`, `NistVault` channel-binding helpers, and the file-transfer stub, while rFerram still requires its own realtime media/input framing and QUIC datagrams (`RFC 9221`).

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| File send | Stub | `crates/rferram-channels/src/snapshot_transfer.rs:55-112`; `initiate_snapshot_transfer` never opens or reads `source_path` or `resolved_snapshot_path`, emits only `ST_INIT:{transfer_id}:{source_path}` (`:105-109`), and has zero callers outside unit tests (`crates/rferram-channels/src/lib.rs:127, 206`, `snapshot_transfer.rs:161`) | R5 |
| File receive | Stub | `crates/rferram-channels/src/snapshot_transfer.rs:115-149`; `handle_incoming_st_bytes` never creates or writes any file on disk, ignores `ST_INIT:` if `transfer_id` is not already in local `active_transfers` (`:121-124`), and discards incoming chunk bytes after updating in-memory counters (`:135-141`) | R5 |
| Integrity check | Stub | `crates/rferram-channels/src/snapshot_transfer.rs:116, 123, 139`; `compute_sha256_bytes(data)` hashes each individual message slice in isolation and overwrites `session.sha256_digest = Some(chunk_sha256)` without computing a streaming file digest or comparing against a sender manifest | R5 |
| Progress accounting | Stub | `crates/rferram-channels/src/snapshot_transfer.rs:96-97, 135-140`; `total_bytes` is hardcoded to `1024 * 1024 * 50` (50 MiB) at `:96` without reading filesystem metadata, and incoming chunks are credited to `transfers.last_mut()` at `:136` without a `transfer_id` header | R5 |
| Snapshot use | Stub | `crates/rferram-channels/src/snapshot_transfer.rs:70-79, 93-99`; `initiate_snapshot_transfer` calls `driver.create_snapshot(&owning_vol)` (`:70`), resolves the path (`:72-74`), and immediately calls `let _ = driver.cleanup_snapshot(&snap)` (`:75`) before any file read while still setting `is_snapshot_locked: true` (`:76, 99`) | R5, R7 |
| Cleanup | Partial | `crates/rferram-channels/src/snapshot_transfer.rs:33, 75, 103` and `vendor/ST_Rust_Age/simply-transfer-snapshots/src/linux.rs:34-78, 95-130`; `active_transfers` entries are never removed (`:103`), `cleanup_snapshot` errors are discarded (`:75`), and failed `lvcreate`/`mount` in `linux.rs:53-57, 73-77` leaks `/mnt/st_snap_<ts>` or the LVM snapshot | R5, R7 |
| Path confinement | Missing | `crates/rferram-channels/src/snapshot_transfer.rs:55-68, 90-108` and `crates/rferram-common/src/models.rs:24`; neither `SimplyTransferChannelHandler` nor `rferram-server` canonicalizes paths, rejects `..` traversal, or enforces `ConnectionProfile::folder_shares` | R3, R5 |
| Resume | Missing | `crates/rferram-channels/src/snapshot_transfer.rs:18-30, 55-149`; `SnapshotTransferSession` tracks no file offset, chunk sequence, or retry state, and `VirtualChannelMessage::SimplyTransfer(Vec<u8>)` has no offset framing | R5, R7 |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-FT-1: `total_bytes` is hardcoded to 50 MiB (`snapshot_transfer.rs:96`). | Confirmed | `crates/rferram-channels/src/snapshot_transfer.rs:96` sets `total_bytes: 1024 * 1024 * 50` on every `SnapshotTransferSession` without inspecting `std::fs::metadata(&source_path)` | FT-01 |
| L-FT-2: the snapshot is cleaned up right after the path is resolved (`:75`), before any read. | Confirmed | `crates/rferram-channels/src/snapshot_transfer.rs:70-77` calls `let _ = driver.cleanup_snapshot(&snap);` at `:75` immediately after `resolve_snapshot_path_for_volume` (`:72-74`) and before `SnapshotTransferSession` is even constructed | FT-02 |
| L-FT-3: no file data is moved; only an `ST_INIT:<id>:<path>` header is sent (`:105`). | Confirmed | `crates/rferram-channels/src/snapshot_transfer.rs:105-111` sends only `format!("ST_INIT:{transfer_id}:{source_path}").into_bytes()` and returns `Ok(transfer_id)`; no code in the workspace ever reads a source file or writes a target file | FT-01 |
| L-FT-4: incoming chunks are credited to the last transfer in the list (`:136`). | Confirmed | `crates/rferram-channels/src/snapshot_transfer.rs:135-140` mutates `transfers.last_mut()` on every non-`ST_INIT:` payload; additionally, `ST_INIT:` (`:120-125`) never inserts a new session on the receiver, so `active_transfers` is empty on a remote peer | FT-03 |
| L-FT-5: a remote `ST_INIT` reaches the Linux snapshot driver, which runs `lvcreate -l 100%FREE` and `sh -c mount/lvremove` (`vendor snapshots src/linux.rs:39, :63, :113`). Trace whether peer-controlled strings reach those commands. | Partly | Refuted that a remote `ST_INIT:` reaches the snapshot driver (`route_channel_message` at `crates/rferram-channels/src/lib.rs:56-58` calls `handle_incoming_st_bytes` at `snapshot_transfer.rs:115-132`, which never invokes `initiate_snapshot_transfer` or `simply_transfer_snapshots`). Confirmed that `LinuxSnapshotDriver` (`vendor/ST_Rust_Age/simply-transfer-snapshots/src/linux.rs:39-48, 61-68, 112-115`) runs `lvcreate -s -n <snap_id> -l 100%FREE <volume_path>` and `sh -c` `mount`/`lvremove -f` with `/dev/mapper/*-...` wildcards when `initiate_snapshot_transfer` is called locally (`owning_vol` is selected from `list_local_volumes()` prefixes in `snapshot_transfer.rs:66-68`, which includes any directory under `/mnt/*` or `/media/*` from `linux.rs:268-302`) | FT-04, ST-04 |
| L-ST-1: vendor copy lacks upstream fixes 2A-2G (verify via item 2). | Confirmed | `git log --stat -- vendor/ST_Rust_Age` shows zero sync commits since initial import `789f472` (2026-10-25), and code inspection of `vendor/ST_Rust_Age/simply-transfer-{core,crypto,snapshots}` confirms all pre-2A..2G defects remain present (`russh_server.rs:351, 385-405, 477-644, 734-804`, `russh_client.rs:28-33`, `engine.rs:526`, `linux.rs:39-115`) | ST-01 |
| L-ST-2: `simply-transfer-core` is a dependency of `rferram-channels` but unused. | Confirmed | `crates/rferram-channels/Cargo.toml:18` declares `simply-transfer-core = { workspace = true }`, while `crates/rferram-channels/src/` has zero references to `simply_transfer_core` | ST-02 |
| L-ST-3: the upstream link design can replace rFerram's SSH/OOB and pairing pieces (verify via item 3). | Confirmed | Upstream `simply-transfer-link` (SSH `st-control/1`, pinned host key, one-time-secret pairing, `SharePolicy`, ticketed QUIC streams) directly replaces `crates/rferram-channels/src/oob_ssh.rs`, `crates/rferram-common/src/vault.rs:100-136`, and `crates/rferram-net/src/endpoint.rs:222-255` | ST-05 |
| L-ST-4: vendor `russh_server.rs` `exec_request` runs `sh -c` / `cmd` (`vendor core src/russh_server.rs:477-641`); confirm it is unreachable from rFerram. | Confirmed | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:477-660` executes `cmd /Q /c` (`:633-638`) or `sh -c` (`:641-643`) on any unmatched SSH `exec_request`, and `:748-789` appends unauthenticated keys to `~/.ssh/authorized_keys`, but `simply_transfer_core` is never imported anywhere in `crates/rferram-*/src/`, making it unreachable at runtime | ST-03 |
| L-ST-5: rFerram mixes `ring` (`quinn`) and `aws-lc-rs` (`russh`) providers; the upstream link design mandates `aws-lc-rs` only. | Confirmed | `Cargo.toml:97-103`, `crates/rferram-net/src/endpoint.rs:65, 133`, and `vendor/ST_Rust_Age/simply-transfer-core/src/{russh_server.rs:310, russh_client.rs:554}` use `ring` for `quinn`/`rustls` alongside `aws-lc-rs` for `russh`, whereas `simply-transfer-link` standardizes on `aws-lc-rs` only | ST-05 |

## Vendor drift

> **Sandbox note:** `/srv/devshare/ST_Rust_Age` lies outside the `/srv/devshare/rFerram` workspace sandbox mount (`PRE_GATE` `git -C /srv/devshare/ST_Rust_Age` exited `1` inside the sandbox and host `read_file` permission timed out without user interaction). Consequently, `vendor/ST_Rust_Age` history and source were audited directly inside `/srv/devshare/rFerram` (`Confidence: Ran` / `Read`), while exact file-level diffs against `/srv/devshare/ST_Rust_Age` at `efeaa92` are marked `Confidence: Suspected` (verifiable on host via `diff -rq vendor/ST_Rust_Age/<crate> /srv/devshare/ST_Rust_Age/<crate> -x target`).

### `git log` history of `vendor/ST_Rust_Age` in rFerram (`Confidence: Ran`)
Running `git log --stat -n 5 -- vendor/ST_Rust_Age` in `/srv/devshare/rFerram` shows only 3 commits have ever touched `vendor/ST_Rust_Age`:
1. `789f472` (`2026-10-25`, *"repush of initial scaffolding"*): Initial vendor import of `simply-transfer-core` (8 files, 4,405 LOC), `simply-transfer-crypto` (5 files, 747 LOC), and `simply-transfer-snapshots` (5 files, 963 LOC).
2. `d5556e8` (`2026-10-27`, *"Security/CI: Refactor to resolve dependency vulnerabilities"*): Modified only `vendor/ST_Rust_Age/simply-transfer-crypto/{Cargo.toml,src/lib.rs}` (`+38/-5` lines) to switch from `keyring` to `keyring-core` + `apple-native-keyring-store` / `windows-native-keyring-store`.
3. `7a7b50e` (`2026-10-28`, *"Phase 7 Completion..."*): Added `license = "GPL-3.0-only"` (`+1` line each) to `vendor/ST_Rust_Age/simply-transfer-{core,crypto,snapshots}/Cargo.toml`.

Zero commits have touched `vendor/ST_Rust_Age/simply-transfer-core/src/*` or `vendor/ST_Rust_Age/simply-transfer-snapshots/src/*` since `789f472`.

### Per-crate drift summary (`vendor/ST_Rust_Age/<crate>` vs. upstream `efeaa92`)
| Crate | Vendored state in `rFerram` (`Read`) | Drift vs. upstream `/srv/devshare/ST_Rust_Age` at `efeaa92` (`Suspected` for live diff; `Read` for unpatched defects in vendor) |
| :--- | :--- | :--- |
| `simply-transfer-core` | Frozen at `789f472` (`src/{engine,firewall,lib,registry,russh_client,russh_server,ssh,ssh_server}.rs`, plus `license` line in `Cargo.toml`). | Missing all upstream `390d737..efeaa92` (`STEP 1B`, `2A–2G`) changes: `russh_server.rs` still has fallback `sh -c`/`cmd /c` execution (`:630-644`), unauthenticated TCP peek JSON key injection + `~/.ssh/authorized_keys` mutation (`:715-804`), host `~/.ssh/authorized_keys` ingestion (`:72-101`), unconfined `data-quic\|` file writes (`:292-405`), and unconfined `simply-transfer-{list,mkdir,exists,hash}\|` commands (`:516-628`); `russh_client.rs:28-33` still accepts all SSH host keys (`Ok(true)`); `engine.rs:526` still accepts `"mock_hash"`. |
| `simply-transfer-crypto` | `789f472` + `d5556e8` (`keyring-core` switch) + `7a7b50e` (`license`). | `src/lib.rs:42-45, 89-92, 110-123` still uses the deterministic `$USER`-derived vault key and `/tmp/simply-transfer-vault` fallback (`COM-01`, slated for upstream `STEP 3A`), and `src/token.rs:15-40` still uses the static XOR `MASK = b"SimplyTransferV2ObfuscationMask!"`. |
| `simply-transfer-snapshots` | Frozen at `789f472` (`src/{lib,linux,macos,windows}.rs`, plus `license` line in `Cargo.toml`). | Missing upstream snapshot hardening: `src/linux.rs:39-48, 61-68, 112-115` still runs `lvcreate -s -n <snap_id> -l 100%FREE` and `sh -c` `mount`/`lvremove -f` with `/dev/mapper/*-...` globs; `src/macos.rs:51-65` still hardcodes `"id-placeholder"` and returns `original_path`; `src/windows.rs:41-48, 91-95` still interpolates unescaped strings into PowerShell WMI commands. |

### Upstream commit / step mapping (`390d737..efeaa92`: `STEP 1B`, `STEP 2A–2G`)
| Upstream Step (`390d737..efeaa92`) | Target area in `ST_Rust_Age` | Present in `vendor/ST_Rust_Age`? | Evidence in `vendor/ST_Rust_Age` (`Confidence: Read`; upstream SHA mapping `Confidence: Suspected`) |
| :--- | :--- | :--- | :--- |
| **STEP 1B** (Audit / baseline documentation) | Upstream plan / audit artifacts | **No** | `vendor/ST_Rust_Age` contains only the 3 crate directories (`simply-transfer-{core,crypto,snapshots}`); no upstream audit/plan files exist in `vendor/ST_Rust_Age/`. |
| **STEP 2A** (Remove arbitrary shell fallback in `exec_request` & shell fallbacks in `engine.rs`) | `simply-transfer-core/src/{russh_server,engine}.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:630-644` still executes `cmd /Q /c` or `sh -c &cmd_str` for any unrecognized `exec_request`, and `engine.rs:609-629` still falls back to `powershell.exe ... New-Item` / `mkdir -p`. |
| **STEP 2B** (Remove unauthenticated OOB JSON `verify` key injection & `~/.ssh/authorized_keys` tampering) | `simply-transfer-core/src/russh_server.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:72-101, 715-813` still peeks for `{` on port 2222, accepts unauthenticated `"action": "verify"` requests, adds `pub_key` to `authorized_keys`, and appends to `~/.ssh/authorized_keys` (`:769-789`). |
| **STEP 2C** (Enforce SSH server host-key verification in `RusshClient`) | `simply-transfer-core/src/russh_client.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_client.rs:28-33` (`ClientHandler::check_server_key`) still ignores `AuthConfig::expected_host_key` (`registry.rs:13`) and returns `Ok(true) // Accept all keys for now`. |
| **STEP 2D** (Enforce path confinement / `SharePolicy` on server file & directory operations) | `simply-transfer-core/src/russh_server.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:292-405, 516-628` (`data-quic\|`, `simply-transfer-list\|`, `simply-transfer-mkdir\|`, `simply-transfer-exists\|`, `simply-transfer-hash\|`) accepts arbitrary absolute or `..` paths with no share-root check. |
| **STEP 2E** (Harden QUIC transfer grant, bind address, and remove `"mock_hash"` validation bypass) | `simply-transfer-core/src/{russh_server,engine}.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:312, 351, 375-378` still uses a 64-bit token on `0.0.0.0:0` without tying the grant to the authenticated SSH peer, and `engine.rs:526` still treats `l.starts_with("mock_hash")` as a valid SHA-256 match. |
| **STEP 2F** (Eliminate `sh -c` wildcards, `-l 100%FREE`, and PowerShell injection in snapshot drivers) | `simply-transfer-snapshots/src/{linux,macos,windows}.rs` | **No** | `vendor/ST_Rust_Age/simply-transfer-snapshots/src/linux.rs:39-48, 61-68, 112-115` still runs `lvcreate ... -l 100%FREE` and `sh -c` `mount`/`lvremove -f` with `/dev/mapper/*-...`; `windows.rs:41-48, 91-95` still interpolates unescaped strings into PowerShell. |
| **STEP 2G** (Crypto provider unification / dependency & key-hygiene cleanup) | `simply-transfer-{core,crypto}` | **No** | `vendor/ST_Rust_Age/simply-transfer-core/Cargo.toml:13, 19-20` and `src/{russh_server.rs:310, russh_client.rs:554}` still install `rustls::crypto::ring::default_provider()` alongside `russh` (`aws-lc-rs`). |

## simply-transfer-link fit

### 1. Component-by-component comparison (`simply-transfer-link` Phase 4 vs. rFerram)
| `simply-transfer-link` Phase 4 capability | What rFerram has today (`Confidence: Read`) | Fit & replacement impact (`Confidence: Read` for rFerram; `Suspected` for unbuilt `simply-transfer-link` internals) |
| :--- | :--- | :--- |
| **SSH control subsystem (`st-control/1`)** | `OobKeyIngestionManager` (`crates/rferram-channels/src/oob_ssh.rs:46-355`) parses unframed `OOB_INGEST_ED25519:` over raw SSH `data` chunks and is never started (`OOB-01`); `RFerramHostDaemon` (`crates/rferram-server/src/daemon.rs:379-459`) uses a broken in-band QUIC `HandshakeMessage` flow (`AUTH-01..03`). | **Direct replacement.** `st-control/1` over SSH replaces `oob_ssh.rs` and the bespoke QUIC auth handshake (`HandshakeMessage::{ServerHello, AuthRequest, AuthResponse}`), providing a single authenticated control plane for both pairing and session/transfer grant negotiation. |
| **Pinned host key & peer trust store** | `oob_ssh.rs:71-76` falls back to a fixed seed (`OOB-03`), `oob_ssh.rs:80` starts with an empty key map (`OOB-04`), and `PeerCertChannelVerifier` (`crates/rferram-net/src/endpoint.rs:222-255`) skips TLS signature verification (`NET-01`). | **Direct replacement.** Host and peer Ed25519 identities are pinned in `simply-transfer-link` and persisted through `simply-transfer-crypto`, eliminating `OobKeyIngestionManager`, `PeerCertChannelVerifier`, and `NistVault::{sign,verify}_channel_bound_challenge` (`crates/rferram-common/src/vault.rs:100-136`). |
| **Pairing via one-time secret** | `oob_ssh.rs:331-354` requires prior SSH auth (bootstrap deadlock, `OOB-04`) yet allows any authed peer to overwrite any `peer_id` (`OOB-02`); `simply-transfer-crypto/src/token.rs:15-40` XORs tokens with a static mask. | **Direct replacement.** One-time-secret pairing bootstraps mutual Ed25519 trust without touching `~/.ssh/authorized_keys` or exposing unauthenticated key-overwrite commands. |
| **`SharePolicy` (authorized share roots & path confinement)** | `ConnectionProfile::folder_shares` (`crates/rferram-common/src/models.rs:24`) is never read outside tests; `SimplyTransferChannelHandler` (`crates/rferram-channels/src/snapshot_transfer.rs:55-149`) performs no path validation (`FT-04`). | **Direct replacement.** `ConnectionProfile::folder_shares` maps directly onto `SharePolicy`, confining both explicit file transfers and clipboard file-drop operations (`R4`, `R5`) to canonicalized share roots. |
| **QUIC data plane with tickets & pinned cert (`aws-lc-rs`)** | `QuicServerListener::bind` (`crates/rferram-net/src/endpoint.rs:120`) mints an unpinned self-signed cert per start (`NET-02`), and `Cargo.toml:97-103` links both `ring` and `aws-lc-rs` (`BASE-03`). | **Direct replacement for session bootstrap.** `st-control/1` issues a short-lived ticket bound to the server's QUIC leaf certificate SHA-256 and standardizes both `russh` and `quinn`/`rustls` on `aws-lc-rs`. |

### 2. Which rFerram pieces `simply-transfer-link` would replace
- **`crates/rferram-channels`**:
  - Delete `src/oob_ssh.rs` (`OobKeyIngestionManager`) entirely and remove the direct `russh` dependency from `crates/rferram-channels/Cargo.toml:16`.
  - Replace `src/snapshot_transfer.rs` (`SimplyTransferChannelHandler`) with a thin adapter that delegates file/directory send, receive, resume, snapshot lifecycle, `SharePolicy` enforcement, and end-to-end SHA-256 verification to `simply-transfer-link` + `simply-transfer-snapshots`.
- **`crates/rferram-net`**:
  - Replace `PeerCertChannelVerifier` (`src/endpoint.rs:222-255`) and unauthenticated `QuicClientTransport::connect` / `QuicServerListener::bind` setup (`src/endpoint.rs:33-195`) with `simply-transfer-link`'s ticketed, cert-pinned `quinn` connection bootstrap on `aws-lc-rs`.
  - Retain (or upstream into `simply-transfer-link`) `VpnSocketFactory` (`src/socket.rs`, after fixing `NET-04` and `NET-05`) and `NetworkMetrics` (`src/metrics.rs`).
- **`crates/rferram-common`**:
  - Remove `NistVault::sign_channel_bound_challenge` and `verify_channel_bound_challenge` (`src/vault.rs:100-136`) and unused `HostCredentials` (`src/models.rs:64-91`), keeping only rFerram-specific session/profile models (`ConnectionProfile`, `RuntimeEnvironment`).

### 3. One QUIC endpoint vs. two (`Confidence: Suspected`)
- In the vendored legacy `simply-transfer-core/src/russh_server.rs:351`, each `data-quic|` transfer binds a fresh ephemeral `0.0.0.0:0` QUIC endpoint, whereas `rferram-server` binds a long-lived QUIC endpoint on port `4433` (`crates/rferram-server/src/main.rs:25-33`).
- Running **two** separate QUIC endpoints (one in `rferram-net` for media/input and a second ephemeral endpoint per file transfer in `simply-transfer-link`) would require multiple UDP firewall/VPN rules, duplicate TLS handshakes, and separate congestion controllers competing on the same link (`Suspected`).
- Instead (`Suspected`), `simply-transfer-link` should expose its authenticated `quinn::Connection` (or accept a shared `quinn::Endpoint` bound via `VpnSocketFactory`) so rFerram runs **one** QUIC endpoint and **one** multiplexed `quinn::Connection` per session: realtime video/audio/input use high-priority streams and datagrams, while bulk file transfers open lower-priority bidirectional streams on the same connection (or a dedicated bulk stream class) with bounded send windows so file transfers do not induce head-of-line queuing or starve 60 fps media.

### 4. What rFerram needs that `simply-transfer-link` lacks (`Confidence: Read` for rFerram; `Suspected` for `simply-transfer-link`)
- **Unreliable QUIC datagrams (`RFC 9221`)**: rFerram sends `InputEvent` and `ControlMessage::FrameAck` as QUIC datagrams (`crates/rferram-net/src/manager.rs:52-81`, `crates/rferram-client/src/bridge.rs:35-42`); `simply-transfer-link` must enable datagram buffers on `quinn::TransportConfig` and expose `send_datagram` / `read_datagram` on the shared connection (`Suspected`).
- **Realtime media & control stream multiplexing**: `simply-transfer-link` is designed for bulk file transfer and control RPCs; it lacks `rferram-protocol`'s `VideoFrameMessage`, `AudioFrameMessage`, `TopologyMessage`, and `ControlMessage::{LossReport, BandwidthEstimate}` framing (`crates/rferram-protocol/src/lib.rs:72-87`) as well as stream priority APIs so bulk file chunks yield to video keyframes and input events (`Suspected`).
- **Cross-platform clipboard synchronization**: `ClipboardPayload::{Text, Image, Files}` polling, injection, and echo suppression (`crates/rferram-channels/src/clipboard.rs`) remain rFerram-specific, though pasting `ClipboardPayload::Files` should trigger a `simply-transfer-link` transfer job.
- **Silent-drop unauthenticated listener behavior (`R8`)**: A standard `russh` TCP listener (`st-control/1`) immediately sends an `SSH-2.0-russh` banner on TCP connect (`OOB-05`); satisfying `R8` ("all other traffic silently ignored") requires either binding the listener strictly to a point-to-point VPN interface (`R1`) or gating the listener behind a single-packet pre-authentication check before speaking SSH/TLS (`Suspected`).

## Findings

### FT-01: `SimplyTransferChannelHandler` is a stub that hardcodes `total_bytes` to 50 MiB, sends only an `ST_INIT:` header, and never reads or writes files
- Severity: High
- Category: stub
- Requirement: R5
- Evidence: `crates/rferram-channels/src/snapshot_transfer.rs:96-111`
```rust
            total_bytes: 1024 * 1024 * 50,
            transferred_bytes: 0,
            sha256_digest: None,
```
- Confidence: Read
- Impact: `initiate_snapshot_transfer` (`:55-112`, which is never called outside unit tests in `crates/rferram-channels/src/{lib,snapshot_transfer}.rs`) never queries file metadata, hardcodes `total_bytes` to 50 MiB (`:96`), never opens or reads `source_path` or `resolved_snapshot_path`, and sends only a single `ST_INIT:{transfer_id}:{source_path}` header (`:105-109`), while `handle_incoming_st_bytes` (`:115-149`) never opens or writes `target_path` on disk.
- Direction: Replace `SimplyTransferChannelHandler`'s stub logic with real streaming file reads and atomic temp-file writes (via `simply-transfer-link`) backed by actual file metadata sizes.

### FT-02: `initiate_snapshot_transfer` destroys the OS snapshot immediately after resolving its path and before any file is read
- Severity: High
- Category: lifecycle
- Requirement: R5, R7
- Evidence: `crates/rferram-channels/src/snapshot_transfer.rs:70-77`
```rust
                let resolved = driver
                    .resolve_snapshot_path_for_volume(&snap, &owning_vol, src_path)
                    .unwrap_or_else(|_| src_path.to_path_buf());
                let _ = driver.cleanup_snapshot(&snap);
```
- Confidence: Read
- Impact: `initiate_snapshot_transfer` calls `driver.cleanup_snapshot(&snap)` at `:75` before returning or reading a single byte from `resolved`, unmounting and deleting the snapshot immediately so `resolved_snapshot_path` points to a non-existent mount while `is_snapshot_locked` is falsely recorded as `true` (`:76, 99`), and discards any cleanup error via `let _ =`.
- Direction: Hold the active `Snapshot` handle open for the duration of the file read stream and invoke `cleanup_snapshot` in a scope/drop guard after all snapshot reads complete or abort.

### FT-03: `handle_incoming_st_bytes` never creates a receiver session on `ST_INIT:`, credits chunks to `active_transfers.last_mut()`, and overwrites `sha256_digest` per chunk
- Severity: High
- Category: stub
- Requirement: R5
- Evidence: `crates/rferram-channels/src/snapshot_transfer.rs:120-125, 135-140`
```rust
            let mut transfers = self.active_transfers.lock().await;
            if let Some(session) = transfers.last_mut() {
                session.transferred_bytes =
                    session.transferred_bytes.saturating_add(data.len() as u64);
```
- Confidence: Read
- Impact: When a remote peer sends `ST_INIT:{transfer_id}:{source_path}`, `handle_incoming_st_bytes` (`:120-125`) only searches `self.active_transfers` (which is empty on the receiving host because only the sender pushed to its own `active_transfers` at `:103`), and on subsequent data chunks it credits `transferred_bytes` to `transfers.last_mut()` (`:136`) without a `transfer_id` and overwrites `session.sha256_digest` with the hash of the single chunk (`:139`) rather than computing or verifying a whole-file SHA-256 digest; furthermore, completed sessions are never removed from `active_transfers`.
- Direction: Register incoming transfer sessions on the receiver from a structured transfer manifest, associate each stream/chunk with its `transfer_id`, maintain a rolling `Sha256` state over the entire file, and verify the final digest against the manifest before committing the file.

### FT-04: `SimplyTransferChannelHandler` and `rferram-server` enforce zero path confinement or `SharePolicy` checks on transfer paths
- Severity: High
- Category: security
- Requirement: R3, R5
- Evidence: `crates/rferram-channels/src/snapshot_transfer.rs:55-68, 105-108` and `crates/rferram-common/src/models.rs:24`
```rust
        let src_path = Path::new(&source_path);
        let volumes = list_local_volumes().unwrap_or_default();
        let owning_vol = resolve_owning_volume(src_path, &volumes)
```
- Confidence: Read
- Impact: Neither `SimplyTransferChannelHandler` (`snapshot_transfer.rs:55-149`) nor `rferram-server` ever reads `ConnectionProfile::folder_shares` (`models.rs:24`), canonicalizes `source_path` / `target_path`, or rejects parent traversal (`..`) and symlinks, and `ST_INIT:{transfer_id}:{source_path}` splits on `:` (`:120`), which corrupts Windows drive paths (`C:\...`) or paths containing colons.
- Direction: Enforce `simply-transfer-link` `SharePolicy` path canonicalization and share-root containment on all source and destination paths, and use structured `postcard` messages instead of colon-delimited strings.

### ST-01: Vendored `vendor/ST_Rust_Age` is frozen at initial scaffolding (`789f472`) and lacks all upstream `STEP 1B` and `2A–2G` fixes
- Severity: High
- Category: security
- Requirement: R3, R5, R11
- Evidence: `vendor/ST_Rust_Age/simply-transfer-core/src/russh_client.rs:28-33` and `vendor/ST_Rust_Age/simply-transfer-core/src/engine.rs:524-526`
```rust
                            let matched = remote_lines
                                .iter()
                                .any(|l| l.starts_with(local_hash) || l.starts_with("mock_hash"));
```
- Confidence: Read
- Impact: Because `vendor/ST_Rust_Age` has not been synced with upstream `/srv/devshare/ST_Rust_Age` (`efeaa92`) since `789f472` (2026-10-25), the vendored crates retain every pre-remediation vulnerability and bug fixed in upstream `2A–2G`, including `check_server_key` accepting all host keys (`russh_client.rs:32`), `engine.rs:526` accepting `"mock_hash"` as a valid SHA-256 verification, unconfined server path operations (`russh_server.rs:292-405, 516-628`), and shell-out snapshot drivers (`linux.rs:39-115`).
- Direction: Update or replace `vendor/ST_Rust_Age` with the remediated upstream `ST_Rust_Age` crates (`efeaa92` + `simply-transfer-link`) once Phase 4 lands.

### ST-02: `simply-transfer-core` is declared in `crates/rferram-channels/Cargo.toml` but completely unused
- Severity: Medium
- Category: structure
- Requirement: R11
- Evidence: `crates/rferram-channels/Cargo.toml:18-20`
```toml
simply-transfer-core = { workspace = true }
simply-transfer-crypto = { workspace = true }
simply-transfer-snapshots = { workspace = true }
```
- Confidence: Read
- Impact: `rferram-channels` compiles `simply-transfer-core` (and its `russh`, `quinn`, `rustls`, `rcgen`, and `walkdir` dependencies) without importing a single item from `simply_transfer_core`, while reimplementing bespoke OOB SSH and snapshot transfer stubs in `crates/rferram-channels/src/{oob_ssh,snapshot_transfer}.rs`.
- Direction: Remove the unused `simply-transfer-core` dependency from `rferram-channels/Cargo.toml` and adopt `simply-transfer-link` for shared transport and file transfer.

### ST-03: Vendored `simply-transfer-core/src/russh_server.rs` contains arbitrary `sh -c` / `cmd /c` execution and unauthenticated `~/.ssh/authorized_keys` injection (unreachable from rFerram today)
- Severity: Medium
- Category: security
- Requirement: R3, R11
- Evidence: `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:633-644, 766-788`
```rust
        #[cfg(not(windows))]
        let output = std::process::Command::new("sh")
            .args(["-c", &cmd_str])
            .output();
```
- Confidence: Read
- Impact: In the vendored `TransferServer`, `exec_request` (`:477-660`) executes any non-prefixed command via `cmd /Q /c` or `sh -c`, and `run_server` (`:715-804`) accepts unauthenticated JSON `{"action":"verify","pub_key":"..."}` on TCP peek and appends the key to `~/.ssh/authorized_keys` (`:769-788`); although unreachable at runtime today because `simply_transfer_core` is never imported by any `rferram-*` crate, the vulnerable code sits compiled in the workspace dependency tree and violates the `.agents/skills/rferram-st-channels` invariant never to touch `~/.ssh/authorized_keys`.
- Direction: Do not import `TransferServer` from the stale vendored `simply-transfer-core`; replace `vendor/ST_Rust_Age` with the remediated upstream version where `exec_request` shell fallback and `~/.ssh/authorized_keys` mutation are removed.

### ST-04: Vendored `simply-transfer-snapshots` drivers execute `lvcreate -l 100%FREE`, wildcard `sh -c` `mount`/`lvremove`, unescaped PowerShell strings, and a broken APFS stub
- Severity: High
- Category: security
- Requirement: R5, R7, R12
- Evidence: `vendor/ST_Rust_Age/simply-transfer-snapshots/src/linux.rs:39-47, 61-67, 112-115`
```rust
        let dev_path = format!("/dev/mapper/*-{}", snapshot.id.replace("_", "--"));
        let rm_output = Command::new("sh")
            .args(["-c", &format!("lvremove -f {}", dev_path)])
```
- Confidence: Read
- Impact: Unlike `simply-transfer-core`, `simply-transfer-snapshots` *is* called by `SimplyTransferChannelHandler::initiate_snapshot_transfer` (`crates/rferram-channels/src/snapshot_transfer.rs:62-75`): on Linux, `LinuxSnapshotDriver` (`linux.rs:24-130`) uses a 1-second-granularity timestamp ID (`st_snap_<secs>`), passes a mount point (or any directory under `/mnt/*` or `/media/*` discovered at `:268-302`) to `lvcreate -s -n <snap_id> -l 100%FREE`, invokes `sh -c` with wildcard `/dev/mapper/*-...` for `mount` (`:63-67`) and `lvremove -f` (`:113-114`), and leaks `/mnt/st_snap_<secs>` or the LV if `lvcreate`/`mount` fails (`:34-77`); on Windows (`windows.rs:41-48, 91-95`), `VssSnapshotDriver` interpolates `drive` and `snapshot.id` into `powershell -Command` without escaping; and on macOS (`macos.rs:51-65`), `ApfsSnapshotDriver` hardcodes `"id-placeholder"` in `mount_point` and returns `original_path` from `resolve_snapshot_path`.
- Direction: Sync the upstream `STEP 2F` snapshot driver fixes to eliminate `sh -c` wildcard commands, pass resolved LV device paths with bounded snapshot sizes, escape/parameterize PowerShell arguments, and implement or fall back cleanly on macOS APFS.

### ST-05: rFerram and vendored `simply-transfer-core` mix `ring` and `aws-lc-rs` crypto providers and duplicate transport/pairing logic slated for `simply-transfer-link`
- Severity: Medium
- Category: structure
- Requirement: R3, R11
- Evidence: `Cargo.toml:97-103` and `vendor/ST_Rust_Age/simply-transfer-core/src/russh_server.rs:310`
```rust
            let _ = rustls::crypto::ring::default_provider().install_default();
```
- Confidence: Read
- Impact: Both `rferram-net` (`crates/rferram-net/src/endpoint.rs:65, 133`) and vendored `simply-transfer-core` (`russh_server.rs:310`, `russh_client.rs:554`) configure `quinn`/`rustls` with `ring` while `russh` uses `aws-lc-rs` (`Cargo.toml:97-103`), and rFerram duplicates SSH key ingestion (`oob_ssh.rs`), channel-binding (`vault.rs`), and QUIC certificate verification (`endpoint.rs`) instead of consuming `simply-transfer-link` (`R11`).
- Direction: Standardize the entire workspace on `aws-lc-rs` and replace rFerram's bespoke SSH/OOB and QUIC auth/pinning code with `simply-transfer-link`.

## Open questions for Claude
- Should rFerram share a single `simply-transfer-link`-bootstrapped `quinn::Endpoint` / `quinn::Connection` for both realtime media/input (streams + `RFC 9221` datagrams) and bulk file transfers (using QUIC stream priorities / flow-control caps), or should bulk file transfers run over a separate per-transfer QUIC connection on the same endpoint?
- Since `/srv/devshare/ST_Rust_Age` lies outside the subagent sandbox mount, please confirm on the host during `validate-step.sh check 1F` whether any additional commits exist in `390d737..efeaa92` beyond `STEP 1B` and `STEP 2A–2G` mapped above.

## Claude validation correction (2026-11-17)

Gemini's sandbox could not read `/srv/devshare/ST_Rust_Age`, so the "Vendor drift" step mapping above was inferred, not diffed. Claude ran the diff on the host (`diff -rq vendor/ST_Rust_Age/<crate> /srv/devshare/ST_Rust_Age/<crate> -x target`; `git -C /srv/devshare/ST_Rust_Age log --oneline 390d737..efeaa92`). This section supersedes the "Upstream commit / step mapping" table and the per-crate drift column wherever they disagree. Gemini's text above is left as written.

### Real diff (Confidence: Ran)
| Crate | Differs from upstream `efeaa92` |
| :--- | :--- |
| `simply-transfer-core` | `Cargo.toml`, `src/{engine,firewall,lib,russh_client,russh_server,ssh}.rs`; vendor still has `src/ssh_server.rs` (deleted upstream); upstream adds `tests/` |
| `simply-transfer-crypto` | `Cargo.toml`, `src/{keys,lib,token}.rs` |
| `simply-transfer-snapshots` | `Cargo.toml` only. **`src/` is identical to upstream.** |

### Real upstream commits `390d737..efeaa92` (Confidence: Ran)
| Step | Commit | What it changed | In vendor? |
| :--- | :--- | :--- | :--- |
| 2A | `a722e9a` | One-time pairing secret on OOB verify (`russh_server.rs`, `crypto/src/token.rs`) | No |
| 2B | `732b963` | UI sends the pairing secret on verify (`simply-transfer-ui` only) | N/A (UI not vendored) |
| 2C | `0c35d7b` | Stop touching OS `~/.ssh/authorized_keys`; quiet auth log; 0600 key store (`p2p_authorized_keys.json`) | No |
| 2D | `43771e4` | Native `os`/`size`/`df` server verbs | No |
| 2E | `8e189f8` | Client and engine use native verbs only (no remote shell strings) | No |
| 2F | `bbb7721` | Unknown `exec_request` runs nothing (removes the `sh -c` / `cmd /c` fallback) | No |
| 2G | `efeaa92` | Write-path guard, QUIC accept timeout, delete `ssh_server.rs` | No |

### Corrections to findings above
- **ST-01:** the conclusion (vendor lacks 2A–2G) stands. The per-step descriptions in the table above are wrong; use this table.
- **ST-03:** the `sh -c` exec fallback and `authorized_keys` mutation **are** fixed upstream (2F, 2C). Syncing removes them.
- **ST-04:** there is **no upstream snapshot fix to sync**. Upstream `simply-transfer-snapshots/src` is byte-identical to the vendor copy, so `lvcreate -l 100%FREE`, the `sh -c` `/dev/mapper/*-` globs, PowerShell interpolation and the APFS `"id-placeholder"` stub are open in upstream too. Direction becomes: fix in upstream ST (new step there), not "sync 2F".
- **Still open upstream at `efeaa92`** (so a sync does not fix them): `russh_client.rs:32` `check_server_key` returns `Ok(true)`; `engine.rs:526` accepts `"mock_hash"`; `rustls::crypto::ring` in `russh_client.rs:433` / `russh_server.rs:315`; static XOR `MASK` in `crypto/src/token.rs:15`; `$USER`-derived vault key + temp-dir fallback in `crypto/src/lib.rs:79,106` (COM-01, upstream STEP 3A). Upstream's own plan removes `russh_client.rs`, `russh_server.rs` and `ssh.rs` once the app moves onto `simply-transfer-link` (Phase 4, STEP 4A–4F, not started).
- **FT-04:** the "colon splits Windows drive paths" sub-claim is wrong: `split_once(':')` splits at the first colon, after the UUID `transfer_id`, so `C:\...` survives. The missing path confinement stands.
- **Open question 2 (extra commits):** none. The range holds exactly 2A–2G.
