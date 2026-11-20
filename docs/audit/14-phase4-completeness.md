# 14 Phase 4 completeness audit
Audited HEAD: 11c252d

## Summary
- **Overall verdict:** Phase 4's core mTLS authentication goal is **implemented and verified** end-to-end (`cargo test --workspace --all-targets --locked` passes; live `perigee-host` + `perigee` run confirms unpinned refusal, trusted mTLS session, and untrusted-client refusal).
- **Goal checklist (G1–G24):** 20 **Real**, 4 **Partial** (`G2`, `G10`, `G11`, `G22`), 0 **Stub**, 0 **Missing**.
- **Security closure:** All 5 Phase 1 Critical findings (`AUTH-01`, `AUTH-02`, `AUTH-03`, `NET-01`, `COM-02`) and all R3/R8 unauthenticated-path blockers (`NET-02`, `NET-04`, `NET-06`, `OOB-01..05`, `CLI-05`, `CLI-08`) are **Closed** or **Moot** at HEAD.
- **Untracked gaps:** **11 UNTRACKED gaps** identified across deferred Phase 4b/hardening items (SSH pairing, host-port rate limiting), release/operator surface (service units, GUI trust setup, `release.yml` tests + CLI bundling), edge-case robustness (`identity.key` atomic rename, `TrustedKeys::save` temp-file race/open handle, `known_hosts` target canonicalization & duplicate-key eviction, client clipboard `INFO` log), and stale docs/remnants (`AGENTS.md`, `perigee-wire-and-quic/SKILL.md`, unused `requires_elevation` / `HostCredentials`).
- **Tracker note:** `neb` CLI was not in PATH in the execution sandbox, so `.tracker/phases/pre_production/` TOML files were inspected read-only to enumerate all scopes and items (`BUG-0001`, `TASK-0001`, `TASK-0002`, `TASK-0039`, `TASK-0040`, `TASK-0058`, `TASK-0064`).

## Goal checklist

| # | Component | Status (Real/Partial/Stub/Missing) | Evidence (file:line) | Tracked by |
| :--- | :--- | :--- | :--- | :--- |
| G1 | Node key at `<config dir>/identity.key`, PKCS#8 DER, created on first use | Real | `crates/perigee-net/src/identity.rs:26,68-94` (`NodeIdentity::load_or_create`); `crates/perigee-host/src/host.rs:255-256,346-348`; `crates/perigee-client/src/connect.rs:81-83` | — |
| G2 | `identity.key` written mode 0600 (Unix) and created atomically (no window where it is world-readable) | Partial | `crates/perigee-net/src/identity.rs:82-93` uses `OpenOptions::create_new(true)` + `.mode(0o600)` (no world-readable window), but writes directly to `identity.key` instead of temp-file + rename (crash mid-write leaves a truncated file) and `fs::read(path)` at `:70` is not zeroized after parsing (`P4-06`) | UNTRACKED |
| G3 | Key struct zeroizes on Drop and redacts the secret in `fmt::Debug` (`AGENTS.md` section 3) | Real | `crates/perigee-net/src/identity.rs:34-37,121-140,290-296` (`NodeIdentity` implements `Zeroize`, `Drop`, and `Debug` with `"<redacted>"`; unit test passes) | — |
| G4 | Public node key shown as 64 lowercase hex chars; parsing rejects other lengths and non-hex | Real | `crates/perigee-net/src/identity.rs:143-167,259-268` (`node_key_to_hex`, `node_key_from_hex` checks `len == 64` and `is_ascii_hexdigit`) | — |
| G5 | Self-signed cert from the node key, SAN `perigee.host` | Real | `crates/perigee-net/src/identity.rs:23,108-118,241-248` (`PERIGEE_SERVER_NAME = "perigee.host"`, `CertificateParams::new`, `self_signed(&key_pair)`) | — |
| G6 | Peer key extracted with `webpki::EndEntityCert` / SPKI prefix `302a300506032b6570032100`, never by scanning DER | Real | `crates/perigee-net/src/identity.rs:29-31,171-187,251-256` (`ED25519_SPKI_PREFIX`, `webpki::EndEntityCert::try_from`, `subject_public_key_info().as_ref().strip_prefix`) | — |
| G7 | Client verifier: accepts only the pinned key; rustls still checks the TLS 1.3 handshake signature (`verify_tls13_signature` not short-circuited) | Real | `crates/perigee-net/src/endpoint.rs:271-325` (`PinnedHostVerifier::verify_server_cert` checks `node_key_from_certificate == self.host_key`; `verify_tls12_signature` and `verify_tls13_signature` delegate to `rustls::crypto::verify_tls12_signature` / `verify_tls13_signature`) | — |
| G8 | Host verifier: client cert required (no anonymous client path) and key must be in `trusted_clients` | Real | `crates/perigee-net/src/endpoint.rs:176-184,330-389` (`with_client_cert_verifier(TrustedClientVerifier)`, `client_auth_mandatory` defaults to `true`, `authorizer.is_authorized(&node_key_from_certificate(end_entity)?)`); tested in `crates/perigee-net/tests/quic_tests.rs:391-422,455-479` | — |
| G9 | Host re-reads `trusted_clients` on every handshake (no restart after `trust add`) | Real | `crates/perigee-host/src/host.rs:308-328,349-351,682-697` (`TrustFileAuthorizer::is_authorized` calls `TrustedKeys::load` on each handshake; unit test proves live add and malformed-file fail-closed) | — |
| G10 | Trust files: line format `"<64 hex> <label>"`, mode 0600, atomic rewrite, a malformed line fails the load | Partial | `crates/perigee-net/src/trust.rs:157-187,203-222,238-337`: format, Unix `0o600`, malformed-line rejection, and `rename` are real, but `TrustedKeys::save` uses a fixed `.tmp` path (`with_extension("tmp")` + `remove_file`) that races concurrent writers and calls `fs::rename` at `:184` before dropping the open `file` handle (`P4-07`) | UNTRACKED |
| G11 | `known_hosts` label = the connect target; the CLI and the UI build the same label for the same target | Partial | `crates/perigee-cli/src/lib.rs:274-280` and `crates/perigee-ui/src/session_controller.rs:355-358` both pass the raw target string (`&args.target`, `&profile.host_address`) to `load_client_auth` (neither trims nor canonicalizes default port `:4433`), and `TrustedKeys::add` (`crates/perigee-net/src/trust.rs:124-138`) filters `entry.key != key`, so pinning the same host key under a second target silently deletes the first target's pin (`P4-05`) | UNTRACKED |
| G12 | Config dir order: `PERIGEE_CONFIG_DIR`, `%APPDATA%\Perigee`, `$XDG_CONFIG_HOME/perigee`, `$HOME/.config/perigee`; error (not a panic, not the CWD) when none resolves | Real | `crates/perigee-net/src/identity.rs:191-209,271-288`; callers return `Err`: `crates/perigee-host/src/host.rs:331-339`, `crates/perigee-cli/src/lib.rs:263-270`, `crates/perigee-ui/src/session_controller.rs:278-283` | — |
| G13 | `--config-dir` on `perigee-host`, `perigee` (all subcommands incl. embedded `perigee host`) | Real | `crates/perigee-host/src/host.rs:187-196,210-219`; `crates/perigee-cli/src/lib.rs:107-112,181-190,227-236` (`perigee host` delegates to `parse_host_args`, covering `host`, `host identity`, `host trust ...`, `connect`, `identity`, `trust ...`) | — |
| G14 | `perigee-host identity` / `trust add|list|remove` | Real | `crates/perigee-host/src/host.rs:124-140,204-289,743-825`; `crates/perigee-host/src/main.rs:40-43`; verified in live run | — |
| G15 | `perigee identity` / `trust add|list|remove` and `connect --host-key` | Real | `crates/perigee-cli/src/lib.rs:64,70-86,174-180,221-260,283-322,461-609`; `crates/perigee-cli/src/main.rs:38-41,70-76`; verified in live run | — |
| G16 | `perigee connect` fails closed with a clear message when no pin exists and no `--host-key` is given | Real | `crates/perigee-client/src/connect.rs:84-94` returns `no pinned host key for '<target>': run \`perigee trust add <target> <KEY>\` ... or pass --host-key`; tested in `crates/perigee-cli/src/lib.rs:533-571` and live run step 5 (exit 1) | — |
| G17 | UI connect loads auth via `load_client_auth` and fails closed without a pin; production UI calls `set_config_dir` or uses the default | Real | `crates/perigee-ui/src/session_controller.rs:274-283,355-365` (`load_client_auth`, fails closed to `SessionConnectOutcome::failed`); `crates/perigee-ui/src/app.rs:245` uses `UiSessionController::new()` (default config dir); tested at `session_controller.rs:1153-1172` | — |
| G18 | In-band auth gone: no `AuthRequest`, `ServerHello`, `AuthMethod`, `auth_methods`, `VaultToken` re-auth, channel-binding code in `crates/` | Real | Grep across `crates/` returns 0 hits for `AuthRequest`, `ServerHello`, `AuthMethod`, `auth_methods`, `channel_bound`, `ed25519_identity`, `CHANNEL_BINDING_DOMAIN_PREFIX`; only `VaultTokenModel` remains in `crates/perigee-common/src/{models,vault,lib}.rs` (tracked by `TASK-0039`, no callers) | — |
| G19 | Accept-any verifier gone from non-test code: no `ServerCertVerified::assertion()` / `HandshakeSignatureValid::assertion()` / `with_no_client_auth` / `dangerous()` outside `#[cfg(test)]` or `tests/` | Real | Non-test hits: only `crates/perigee-net/src/endpoint.rs:118` (`.dangerous()`) and `:286` (`ServerCertVerified::assertion()`), inside `PinnedHostVerifier` strictly gated on `node_key_from_certificate(end_entity)? == self.host_key` with real rustls TLS signature checks (`:292-318`). Test-only hits: `crates/perigee-net/tests/quic_tests.rs:32,41,50,75,77` (`AcceptAnyServerCert` + `connect_raw_without_client_cert`). Zero `HandshakeSignatureValid::assertion()` or `with_no_client_auth` outside `tests/`. | — |
| G20 | `aws-lc-rs` is the only rustls/quinn/rcgen provider in the build graph | Real | `cargo tree --locked -e features -i ring` outputs `warning: nothing to print.` (native targets). `cargo tree --locked --target all -e features -i ring` shows `ring v0.17.14` solely via `quinn-proto`'s `wasm32_unknown_unknown_js` target-gated dep. | — |
| G21 | Real host and real client loopback test runs over mTLS with a trusted client, plus a test that an untrusted client and a wrong host key are both refused | Real | `crates/perigee-cli/tests/loopback.rs:31-171` (`test_real_host_and_client_full_lifecycle` passes); untrusted client & wrong host key refusal tested in `crates/perigee-net/tests/quic_tests.rs:391-479` and `crates/perigee-client/src/connect.rs:454-483` (all pass). | — |
| G22 | Wire: `ClientHello` no longer carries credentials; no remaining wire variant or field exists only for the deleted auth | Partial | `crates/perigee-protocol/src/lib.rs:144-157`: `ClientHello` has only `protocol_version`, `client_id`, `client_name`, `supported_codecs`. However `AuthResponse` retains `requires_elevation: bool` (always `false`, leftover from removed Phase 2 elevation) and `session_token: Option<String>` (carries `session_id` UUID) (`P4-09`). | UNTRACKED |
| G23 | `README` documents the trust setup and its commands match the real `USAGE` text exactly | Real | `README.md:17,45-76` commands (`identity`, `trust add 127.0.0.1:4433 <HOST_KEY>`, `trust add <CLIENT_KEY> laptop`, `connect 127.0.0.1:4433 --for 10`) match `HOST_USAGE` (`crates/perigee-host/src/host.rs:24-52`) and `USAGE` (`crates/perigee-cli/src/lib.rs:14-54`) verbatim in flag names and argument order. | — |
| G24 | Errors on trust/identity failures do not print secret key bytes | Real | Audited `crates/perigee-net/src/{identity,trust}.rs`, `crates/perigee-host/src/host.rs`, `crates/perigee-cli/src/lib.rs`, `crates/perigee-client/src/connect.rs`: errors print only file paths, line numbers, or user-supplied public key/label text; `pkcs8_der` is never formatted in any `bail!`, `anyhow!`, `.with_context`, or `tracing` call, and `NodeIdentity::fmt` prints `"<redacted>"`. | — |

## Finding closure

Every finding listed under R3 or R8 (blocking and supporting) in `docs/audit/11-traceability.md`, plus `PROTO-01` and `CLI-05`, using `docs/audit/12-claude-validation.md` severities where adjusted:

| Finding | Sev | Disposition (Closed/Open/Moot/Out of Phase 4) | Evidence or closing step | Tracked by |
| :--- | :--- | :--- | :--- | :--- |
| AUTH-01 | Critical | Closed | Bare `ClientHello` can no longer reach `handle_handshake` without passing mTLS `TrustedClientVerifier` (`crates/perigee-net/src/endpoint.rs:176-184,335-389`; `crates/perigee-host/src/host.rs:349-361`). Steps 4I, 4K, 4P. | — |
| AUTH-02 | Critical | Closed | In-band `AuthMethod::Ed25519Signature` deleted (`crates/perigee-host/src/daemon.rs:307-333`; `crates/perigee-protocol/src/lib.rs:144-157`); peer keys verified via TLS 1.3 handshake signature + `TrustedKeys` allowlist (`endpoint.rs:340-382`). Steps 4K, 4M. | — |
| AUTH-03 | Critical | Closed | `SystemUserPassword` and `VaultToken` auth/re-auth arms deleted (`crates/perigee-host/src/daemon.rs:307-333`, `crates/perigee-protocol/src/lib.rs:144-157`). Steps 4K, 4M. | — |
| NET-01 | Critical | Closed | `PeerCertChannelVerifier` and legacy `bind`/`connect` deleted (`crates/perigee-net/src/endpoint.rs:1-390`); `PinnedHostVerifier` pins `host_key` and delegates TLS 1.2/1.3 signatures to rustls (`endpoint.rs:276-325`). Steps 4G, 4P. | — |
| COM-02 | Critical | Closed | `sign_channel_bound_challenge` / `verify_channel_bound_challenge` and `CHANNEL_BINDING_DOMAIN_PREFIX` deleted (`crates/perigee-common/src/vault.rs:1-66`). Step 4Q. | — |
| COM-01 | High | Moot (in Phase 4) / Out of Phase 4 | `NistVault` is no longer on any auth path (`crates/perigee-common/src/vault.rs:1-66`, 0 non-test callers); node keys live in `identity.key` (mode `0600`). Upstream `simply-transfer-crypto::SecureStorage` fate is tracked for Phase 4b. | `TASK-0039` |
| NET-02 | High | Closed | Ephemeral per-start server certs replaced by persistent `NodeIdentity::load_or_create` in `<config dir>/identity.key` (`crates/perigee-net/src/identity.rs:68-94`; `crates/perigee-host/src/host.rs:346-348`). Steps 4E, 4I, 4P. | — |
| NET-04 | High (Win/macOS) / Low (Linux) | Closed | `SO_REUSEADDR`/`SO_REUSEPORT` removed from `VpnSocketFactory` (`crates/perigee-net/src/socket.rs`). Step 2D. | — |
| AUTH-04 | High | Closed | `AuthRequest` credentials deleted (`crates/perigee-protocol/src/lib.rs:144-157`); `daemon.rs:308-311` logs only credential-free `ClientHello`. Steps 4K, 4M. | — |
| PROTO-04 | High | Closed | `AuthRequest` and `ServerHello` deleted (`crates/perigee-protocol/src/lib.rs:144-157`); no secrets remain in `HandshakeMessage` logged at `daemon.rs:308-311` or `client/src/session.rs:260`. Step 4M. | — |
| OOB-01 | High | Moot | Legacy `oob_ssh.rs` deleted in Step 2B (`crates/perigee-channels/src/` has clipboard only); SSH pairing deferred to Phase 4b (`simply-transfer-link`). | UNTRACKED (`P4-01`) |
| OOB-02 | High | Moot | `oob_ssh.rs` deleted in Step 2B. | — |
| OOB-03 | High | Moot | `oob_ssh.rs` deleted in Step 2B. | — |
| OOB-04 | High | Moot | `oob_ssh.rs` deleted in Step 2B. | — |
| OOB-05 | High | Moot | `oob_ssh.rs` deleted in Step 2B; SSH pairing listener belongs to Phase 4b (`simply-transfer-link`). | UNTRACKED (`P4-01`) |
| CLI-05 | High | Closed | `ServerHello` deleted (4M); `ClientSessionPipeline` transitions `Connecting -> Authenticating -> Active` and `AuthResponse { success: false, .. }` transitions to `SessionState::Terminated` (`crates/perigee-client/src/session.rs:261-276`). Steps 3D, 4M. | — |
| CLI-08 | High | Closed | Gap detection bounded to `MAX_LOSS_REPORT_FRAMES = 64` with `.take()` and saturating `expected_frame_id` (`crates/perigee-client/src/session.rs:288-311`). Step 3B. | — |
| FT-04 | High | Moot | Legacy `snapshot_transfer.rs` deleted in Step 2B; file transfer is Phase 6 (`simply-transfer-link`). | — |
| ST-01 | High | Out of Phase 4 | Vendored `ST_Rust_Age` refresh + `simply-transfer-link` adoption is Phase 4b (`TASK-0001` covers unused `[patch]` entries; `simply-transfer-core` is no longer in the crate graph). | `TASK-0001` / UNTRACKED (`P4-01`) |
| NET-06 | High | Closed | Host defaults to `127.0.0.1:4433` (`crates/perigee-host/src/host.rs:18`), fails closed on missing VPN interface, and rejects untrusted peers at TLS handshake (`endpoint.rs:340-354`). Steps 2D, 2H, 4I. | — |
| PROTO-01 | High | Out of Phase 4 | Auth handshake variants (`ServerHello`, `AuthRequest`, `AuthMethod`) deleted in 4M and `Disconnect` wired in 3E; remaining unwired media/topology/clipboard variants belong to Phases 5–6 (`crates/perigee-protocol/src/lib.rs:144-157`). | — |
| AUTH-05 | Medium | Open | Handshake now requires a trusted mTLS client before `serve_connection` runs, mitigating unauthenticated 16 MiB stream buffering; however the host port still has no rate limiting, no connection/session cap, and no handshake timeout before `recv_uni_message` (`crates/perigee-host/src/daemon.rs:417-420`, `host.rs:381-413`) (`P4-02`). | UNTRACKED |
| BASE-05 | Low | Open (partial) | `russh` removed from `Cargo.lock` in Step 2B (`RUSTSEC-2026-0495` closed); `rustix 1.1.5` (`RUSTSEC-2026-0486`, unreachable xattr) remains at `Cargo.lock:2510-2511`. Out of Phase 4 (lockfile freeze). | — |
| BASE-03 | Low | Closed | `ring` removed from native build graph; `aws-lc-rs` is the sole provider (`Cargo.toml`, `crates/perigee-net/src/endpoint.rs:111,175`). Step 4D. | — |
| PROTO-02 | Medium | Out of Phase 4 | `WireMessage` framing/enum layout hygiene outside auth; not in Phase 4 scope. | — |
| PROTO-03 | Medium | Closed (client) / Out of Phase 4 (stream cap) | Client `LossReport` gap allocation bounded to 64 (`crates/perigee-client/src/session.rs:290-292`, Step 3B) and host/client `LossReport` logs bounded (Steps 3A, 4A); postcard varint expansion cap on trusted streams is outside Phase 4. | — |
| COM-03 | Medium | Moot (in Phase 4) | `NistVault` has zero non-test callers after 4Q (`crates/perigee-common/src/vault.rs`); fate tracked for Phase 4b. | `TASK-0039` |
| COM-04 | Medium | Moot (in Phase 4) | `VaultTokenModel` auth comparison deleted in 4K; `VaultTokenModel` itself has no non-test callers. | `TASK-0039` |
| LIFE-08 | Medium | Closed | `bitrate_kbps_from_bytes_per_sec` uses `saturating_mul(8)` (`crates/perigee-host/src/daemon.rs:256-258`). Step 3A. | — |
| ST-03 | Medium | Moot | `simply-transfer-core` removed from workspace crate graph in Step 2B; unused `[patch]` cleanup tracked by `TASK-0001`. | `TASK-0001` |
| ST-05 | Medium | Closed (provider) / Out of Phase 4 (link) | Crypto provider unified on `aws-lc-rs` (Step 4D) and manual mTLS is on Perigee's single QUIC endpoint; `simply-transfer-link` pairing is Phase 4b. | UNTRACKED (`P4-01`) |
| CLI-10 | Medium | Open | `AuthRequest` credentials are gone, but `crates/perigee-client/src/session.rs:385` still logs `info!(?vc, "Virtual channel payload received")` (clipboard text/image bytes) at `INFO` level (`P4-10`). | UNTRACKED |
| UI-05 | Medium | Out of Phase 4 | UI stub drawers/modals/settings (Phase 9); however the absence of UI trust/identity views blocks GUI-only Phase 4 operation (`P4-04`). | UNTRACKED (`P4-04`) |

## Deferred items

| Item | Source | Tracked by |
| :--- | :--- | :--- |
| SSH pairing via `simply-transfer-link` (Phase 4b) | Phase 4 header ("Not in Phase 4"), `README.md:17,86` | UNTRACKED (`P4-01`) |
| Host-port rate limiting / brute-force protection | Phase 4 header ("Not in Phase 4"), `README.md:45` | UNTRACKED (`P4-02`) |
| `NistVault` session-token store fate after 4Q | Phase 4 header ("Not in Phase 4") | `TASK-0039` |
| Unused `[patch]` entries for `simply-transfer-core` and `simply-transfer-snapshots` | Phase 4 header ("Kept for Phase 4b") | `TASK-0001` |
| Await `Disconnect` delivery instead of sleeping 20 ms | Phase 4 header ("Kept for Phase 4b") | `BUG-0001` |
| Print host node key instead of cert SHA-256 in `perigee connect` | `crates/perigee-cli/src/main.rs:77-81` | `TASK-0040` |
| Remove unused `HostOptions::from_env` | Completed table row 4C (`crates/perigee-host/src/host.rs:97-99`) | `TASK-0058` |
| Research headless capture and `cosmic-greeter` seat following (Phase 5) | `README.md:18,87` | `TASK-0002` |
| Clipboard monitor loop wiring & file-list clipboard sync (Phase 6) | `README.md:21,88` | Out of Phase 4 (Phase 6) |
| Real capture, encode/decode, input, audio, packaging split, COSMIC GUI (Phases 5–9) | `README.md:18-25,87-91` | Out of Phase 4 (Phases 5–9) |

## Release surface

| Area | Status | Evidence | Tracked by |
| :--- | :--- | :--- | :--- |
| `packaging/linux/perigee-host.service` | Broken for Phase 4 ops | `packaging/linux/perigee-host.service:6-13`: no `User=` (runs as `root`), no `ConfigurationDirectory=` or `PERIGEE_CONFIG_DIR`, retains `AmbientCapabilities=CAP_NET_RAW CAP_SYS_ADMIN`. Under systemd, if `HOME` is unset `default_config_dir()` returns `None` and startup fails; if `HOME=/root`, `identity.key`/`trusted_clients` land in `/root/.config/perigee`, requiring `sudo` + matching dir for `trust add` (`P4-03`). Also `packaging/linux/package-linux.sh:77,104,173,230` omits the `perigee` CLI binary (`P4-11`). | UNTRACKED |
| `packaging/macos/com.theinfomoose.perigee-host.plist` | Broken for Phase 4 ops | `packaging/macos/com.theinfomoose.perigee-host.plist:11-15`: sets only `PERIGEE_BIND_ADDR=127.0.0.1:4433`; no `PERIGEE_CONFIG_DIR`, no `UserName`. As a LaunchDaemon, `HOME` is unset or `/var/root`; as a LaunchAgent, config lands in `~/.config/perigee` rather than `~/Library/Application Support/Perigee` (`P4-03`). Also `packaging/macos/package-macos.sh:37,54` omits the `perigee` CLI binary (`P4-11`). | UNTRACKED |
| `packaging/windows/perigee.wxs` | Incomplete for Phase 4 ops | `packaging/windows/perigee.wxs:27-50` and `packaging/windows/package-windows.ps1:34,54`: bundle `perigee-ui.exe` and `perigee-host.exe` only — omit `perigee.exe` CLI (`P4-11`), define no `<ServiceInstall>`, and set no `PERIGEE_CONFIG_DIR` (manual runs use `%APPDATA%\Perigee`; a future LocalSystem service would use `systemprofile\AppData\Roaming\Perigee`) (`P4-03`). | UNTRACKED |
| `.github/workflows/ci.yml` | OK | `.github/workflows/ci.yml:125-132`: runs `cargo test --workspace --all-targets --locked` (covers all mTLS unit, integration, and loopback tests) plus `perigee-host` smoke test on `ubuntu-latest`, `windows-latest`, `macos-latest`. | — |
| `.github/workflows/release.yml` | Gap | `.github/workflows/release.yml:41-85,186-205`: runs `cargo fmt`, `clippy`, `audit`, `deny`, release build, and `PERIGEE_SMOKE_TEST=1`, but never runs `cargo test` (no mTLS tests on release tags) (`P4-11`). | UNTRACKED |
| `.agents/skills/**` and `AGENTS.md` | Stale | `.agents/skills/perigee-wire-and-quic/SKILL.md:5,16,17-21,40,41` still documents deleted `NistVault` channel binding, `ed25519-dalek`, `ring::default_provider()`, and `PeerCertChannelVerifier`, omitting `NodeIdentity`/`TrustedKeys`/`bind_mtls`/`connect_mtls`. `AGENTS.md:19` lists unused `simply-transfer-core`/`-snapshots` (`TASK-0001`), and `AGENTS.md:27` lists `HostCredentials`/`VaultTokenModel` without `NodeIdentity` (`P4-08`). | UNTRACKED (`P4-08`) / `TASK-0001` |
| `CONTRIBUTING.md` and `README.md` | OK | `CONTRIBUTING.md:1-78` has no stale auth statements. `README.md:17,45-76,86` accurately reflects Phase 4 mTLS and matches `USAGE`/`HOST_USAGE` (minor: `README.md:16` CLI row omits `--config-dir`/`identity`/`trust`, which are documented at `:47-64`). | — |
| `perigee-ui` GUI trust setup | Missing | `crates/perigee-ui/src/` has no view, modal, or drawer to display the client's `NodeIdentity` public key, pin a host key in `known_hosts`, or trust a client in `trusted_clients`; a GUI-only user must use the CLI (`P4-04`). | UNTRACKED |

## Live run

Ran against `target/debug/perigee-host` and `target/debug/perigee` built at HEAD `11c252d` using isolated temp config dirs `H`, `C`, `U` under `$(mktemp -d)` (host killed and temp dirs deleted after completion; all 64-hex keys redacted to `<64 hex>`):

1. `perigee-host identity --config-dir H` -> **exit 0**, printed 64-char hex key (`<64 hex>`).
2. `perigee identity --config-dir C` -> **exit 0**, printed 64-char hex key (`<64 hex>`).
3. `perigee-host trust add <64 hex> testuser --config-dir H` -> **exit 0**, `trusted <64 hex> as testuser`.
4. Started `perigee-host --bind 127.0.0.1:47433 --config-dir H` in background.
5. `perigee connect 127.0.0.1:47433 --config-dir C --for 2` (unpinned) -> **exit 1**:
   `Error: no pinned host key for '127.0.0.1:47433': run \`perigee trust add 127.0.0.1:47433 <KEY>\` with the key \`perigee-host identity\` prints, or pass --host-key`
6. `perigee trust add 127.0.0.1:47433 <64 hex> --config-dir C` -> **exit 0**, `pinned <64 hex> for 127.0.0.1:47433`.
7. `perigee connect 127.0.0.1:47433 --config-dir C --for 2` (pinned + trusted) -> **exit 0**:
   - `INFO perigee_net::endpoint: Mutual-TLS QUIC connection established with 127.0.0.1:47433`
   - `connected to 127.0.0.1:47433; host certificate sha256 <64 hex>`
   - `state=Active frames=57 fps=0.0 rtt_ms=0.0`
   - `disconnected`
   - Host log: `Admitted client session ... peer_node_key=<64 hex>`, followed on disconnect by `Reaped stopped session and closed QUIC connection ... ended_by="host session stop"`.
8. `perigee trust add 127.0.0.1:47433 <64 hex> --config-dir U` then `perigee connect 127.0.0.1:47433 --config-dir U --for 2` (untrusted client) -> **exit 1**:
   - Client: `host rejected the session: connection closed: aborted by peer: the cryptographic handshake failed: error 49: invalid peer certificate: ApplicationVerificationFailure`
   - Host log: `WARN perigee_net::endpoint: Failed to complete incoming QUIC handshake from 127.0.0.1:46006: the cryptographic handshake failed: error 49: invalid peer certificate: ApplicationVerificationFailure`
9. Sent `SIGTERM` to host -> `Received SIGTERM, shutting down`, `QUIC endpoint drained cleanly`, `host stopped`.

## Leads

| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-P4-1 | Confirmed | `STEPPED_IMPLEMENTATION_PLAN.md:1670-1673` lists SSH pairing and host-port rate limiting as "Not in Phase 4"; `.tracker/phases/pre_production/` has zero open/pending items for either (`BUG-0001`, `TASK-0001`, `TASK-0002`, `TASK-0039`, `TASK-0040`, `TASK-0058`, `TASK-0064`). | `P4-01`, `P4-02` |
| L-P4-2 | Confirmed | `packaging/linux/perigee-host.service:6-13` has no `User=` and no `PERIGEE_CONFIG_DIR` / `ConfigurationDirectory=`; `crates/perigee-net/src/identity.rs:197-209` returns `None` when `PERIGEE_CONFIG_DIR`, `XDG_CONFIG_HOME`, and `HOME` are unset, or resolves to `/root/.config/perigee` under root. | `P4-03` |
| L-P4-3 | Confirmed | `.agents/skills/perigee-wire-and-quic/SKILL.md:5` ("NIST vault, Ed25519 TLS 1.3 channel binding"), `:17-21`, `:40` (`ring::default_provider()`), and `:41` (`PeerCertChannelVerifier`) all describe deleted Phase 1/4 code. | `P4-08` |
| L-P4-4 | Confirmed | `AGENTS.md:27` names only `HostCredentials` and `VaultTokenModel` and omits `NodeIdentity`. `NodeIdentity` does implement `Zeroize`, `Drop`, and redacted `Debug` (`crates/perigee-net/src/identity.rs:121-140`), while `HostCredentials` (`crates/perigee-common/src/models.rs:64`) and `VaultTokenModel` (`:95`) have zero non-test callers (`TASK-0039` tracks `NistVault` / `VaultTokenModel`, not `HostCredentials`). | `P4-08`, `P4-09` |
| L-P4-5 | Confirmed | Grep across `crates/perigee-ui/src/` confirms `session_controller.rs:357` calls `load_client_auth`, but no UI widget, modal, or drawer displays `NodeIdentity::public_key_hex` or edits `known_hosts` / `trusted_clients`. | `P4-04` |
| L-P4-6 | Confirmed | `crates/perigee-client/src/connect.rs:86-88` looks up `TrustedKeys::find_by_label(target)` using the raw target string, whereas `resolve_host_addr` (`connect.rs:30-48`) trims whitespace and appends `:4433` when omitted. Worse, `TrustedKeys::add` (`crates/perigee-net/src/trust.rs:124-138`) filters `entry.key != key && entry.label != label`, so pinning the same host key under two targets (`host.example.com:4433` then `192.0.2.10:4433`) silently deletes the first pin (reproduced with `perigee trust add`/`list`). | `P4-05` |
| L-P4-7 | Confirmed | `AGENTS.md:19` still lists `simply-transfer-core` and `simply-transfer-snapshots` as vendored crates; `cargo tree --locked` warns both `[patch]` entries are unused (tracked by `TASK-0001` in `inbox`, while `AGENTS.md` itself is untracked). | `P4-08` (`TASK-0001`) |

## Findings

### P4-01: SSH pairing via `simply-transfer-link` (Phase 4b) is untracked in Nebulous
- Severity: High
- Category: gap
- Requirement: R3, R8, R11
- Evidence: `README.md:17` (`Keys are exchanged by hand for now; SSH pairing via simply-transfer-link is planned for Phase 4b.`) and `.tracker/phases/pre_production/` (0 matching items).
- Confidence: Read
- Impact: Deferred Phase 4b SSH pairing and `simply-transfer-link` adoption have no tracker item and risk being dropped during phase transitions.
- Direction: File a pending `feat` item in `perigee_net` for Phase 4b SSH pairing via `simply-transfer-link`.
- Tracked by: UNTRACKED

### P4-02: Host QUIC port lacks rate limiting, connection caps, and handshake timeout (`AUTH-05`)
- Severity: Medium
- Category: security
- Requirement: R3, R8
- Evidence: `crates/perigee-host/src/daemon.rs:417-420` (`let initial_wire = conn.recv_uni_message().await...`) and `crates/perigee-host/src/host.rs:381-413` (unbounded `tokio::spawn` per accepted connection, no rate limit).
- Confidence: Read
- Impact: An attacker on a reachable interface can flood TLS handshakes without rate limiting, and a misbehaving trusted peer can hold open an idle stream indefinitely before `ClientHello`.
- Direction: Track rate limiting, concurrent connection/session caps, and a `ClientHello` read timeout on the host accept path.
- Tracked by: UNTRACKED

### P4-03: OS service definitions lack non-root user and explicit `PERIGEE_CONFIG_DIR`
- Severity: High
- Category: gap
- Requirement: R9, R12
- Evidence: `packaging/linux/perigee-host.service:6-13` (no `User=`, no `PERIGEE_CONFIG_DIR`, `AmbientCapabilities=CAP_NET_RAW CAP_SYS_ADMIN`); `packaging/macos/com.theinfomoose.perigee-host.plist:11-15` (no `PERIGEE_CONFIG_DIR`); `packaging/windows/perigee.wxs:42-44` (no `<ServiceInstall>`).
- Confidence: Read
- Impact: Running `perigee-host` as a system service either fails to start when `HOME` is unset or stores `identity.key`/`trusted_clients` under root's home where operators cannot manage trust cleanly.
- Direction: Configure a dedicated least-privilege service account and explicit config directory (`ConfigurationDirectory=` / `PERIGEE_CONFIG_DIR`) across Linux, macOS, and Windows service packaging.
- Tracked by: UNTRACKED

### P4-04: `perigee-ui` has no UI to view node identity or manage trust files
- Severity: High
- Category: gap
- Requirement: R3, R10
- Evidence: `crates/perigee-ui/src/session_controller.rs:355-358` (`load_client_auth(&dir, &profile.host_address, None)`) with 0 UI widgets referencing `NodeIdentity`, `known_hosts`, or `trusted_clients` in `crates/perigee-ui/src/`.
- Confidence: Read
- Impact: A GUI-only user cannot discover their node key or pin a host key without dropping to the `perigee` CLI.
- Direction: Add an identity/trust management section to `perigee-ui` (view node key, pin/unpin host keys in `known_hosts`, and manage `trusted_clients`).
- Tracked by: UNTRACKED

### P4-05: `known_hosts` does not canonicalize targets and silently evicts prior targets for the same host key
- Severity: Medium
- Category: lifecycle
- Requirement: R3
- Evidence: `crates/perigee-client/src/connect.rs:86-88` (`find_by_label(target)` vs `resolve_host_addr` trimming/appending `:4433` at `:31,44-48`), `crates/perigee-net/src/trust.rs:129` (`.filter(|entry| entry.key != key && entry.label != label)`), and `crates/perigee-cli/src/lib.rs:318` (`pins.remove(&entry.key)?`).
- Confidence: Ran (`perigee trust add host.example.com:4433 <KEY>` then `perigee trust add 192.0.2.10:4433 <KEY>` -> `perigee trust list` shows only `192.0.2.10:4433`)
- Impact: Pinning `host.example.com:4433` and connecting to `host.example.com` fails closed, and pinning the same host key under a second target (e.g. DNS name and IP) silently deletes the first target's pin.
- Direction: Canonicalize `known_hosts` target labels (trim whitespace, normalize default `:4433` port), allow multiple target labels per host key in `known_hosts`, and remove by label in `perigee trust remove`.
- Tracked by: UNTRACKED

### P4-06: `NodeIdentity::load_or_create` writes directly without temp-file rename and leaves read DER unzeroized
- Severity: Low
- Category: hygiene
- Requirement: R3
- Evidence: `crates/perigee-net/src/identity.rs:70-73` (`let der = fs::read(path)?;` never zeroized) and `crates/perigee-net/src/identity.rs:82-93` (`options.write(true).create_new(true).mode(0o600)` written directly to `path`).
- Confidence: Read
- Impact: A crash between `open` and `write_all` leaves a truncated `identity.key` that breaks subsequent starts, and the temporary `Vec<u8>` holding PKCS#8 DER bytes on load is not explicitly zeroized.
- Direction: Write `identity.key` via a `0o600` temp file + atomic rename (or `link`+`unlink`) and zeroize the `der` buffer in `load_or_create` after parsing.
- Tracked by: UNTRACKED

### P4-07: `TrustedKeys::save` uses a fixed `.tmp` filename and renames while the file handle is still open
- Severity: Low
- Category: hygiene
- Requirement: R3, R12
- Evidence: `crates/perigee-net/src/trust.rs:171-185` (`let tmp = self.path.with_extension("tmp"); let _ = fs::remove_file(&tmp); ... file.sync_all()?; fs::rename(&tmp, &self.path)?;`).
- Confidence: Read (fixed `.tmp` race and open handle before `rename`); Suspected (Windows/non-NTFS rename failure while `file` is still open; would be proven by running `trust` tests on Windows)
- Impact: Concurrent writers collide on the fixed `.tmp` path (`remove_file` can unlink another writer's in-flight file), and keeping `file` open across `fs::rename` risks sharing-violation failures on Windows or AV-scanned filesystems.
- Direction: Create a unique temp file in the same directory and `drop(file)` after `sync_all()` before calling `fs::rename`.
- Tracked by: UNTRACKED

### P4-08: `AGENTS.md` and `perigee-wire-and-quic/SKILL.md` still document deleted auth and crypto
- Severity: Low
- Category: hygiene
- Requirement: R11
- Evidence: `.agents/skills/perigee-wire-and-quic/SKILL.md:5,21,40-41` (`Ed25519 TLS 1.3 channel binding`, `ring::default_provider()`, `PeerCertChannelVerifier`) and `AGENTS.md:19,27` (`simply-transfer-core`/`-snapshots`; omits `NodeIdentity`).
- Confidence: Read
- Impact: Future agents following `AGENTS.md` and `perigee-wire-and-quic/SKILL.md` receive contradicted instructions about `ring`, channel binding, and `PeerCertChannelVerifier`.
- Direction: Update `AGENTS.md` and `.agents/skills/perigee-wire-and-quic/SKILL.md` to document `NodeIdentity`, `TrustedKeys`, `aws-lc-rs`, and `bind_mtls`/`connect_mtls`.
- Tracked by: UNTRACKED

### P4-09: Stale auth remnants (`requires_elevation`, `"none (legacy transport)"`, unused `HostCredentials`)
- Severity: Low
- Category: structure
- Requirement: R3
- Evidence: `crates/perigee-protocol/src/lib.rs:155` (`requires_elevation: bool`), `crates/perigee-host/src/daemon.rs:448` (`"none (legacy transport)"`), `crates/perigee-common/src/models.rs:64` (`pub struct HostCredentials`, test-only).
- Confidence: Read
- Impact: Dead fields, misleading log fallback text, and unused credential structs clutter the protocol and host/common crates after Phase 4 cleanup.
- Direction: Remove `requires_elevation` from `AuthResponse`, update the `daemon.rs:448` fallback string, and delete unused `HostCredentials` (or fold into `TASK-0039`).
- Tracked by: UNTRACKED

### P4-10: Client logs full `VirtualChannelMessage` payloads at `INFO` level (`CLI-10`)
- Severity: Medium
- Category: security
- Requirement: R3, R4
- Evidence: `crates/perigee-client/src/session.rs:384-386` (`WireMessage::Channel(vc) => { info!(?vc, "Virtual channel payload received"); }`).
- Confidence: Read
- Impact: Incoming clipboard text or image bytes (potentially passwords or sensitive user data) are written in plaintext to client `INFO` logs.
- Direction: Log only the channel variant and byte length at `debug!` instead of dumping `?vc` at `info!`.
- Tracked by: UNTRACKED

### P4-11: `release.yml` skips `cargo test` and all OS packaging scripts omit the `perigee` CLI
- Severity: Medium
- Category: gap
- Requirement: R2, R12
- Evidence: `.github/workflows/release.yml:41-85,186-205` (no `cargo test` step); `packaging/linux/package-linux.sh:77,104,173,230`, `packaging/macos/package-macos.sh:37,54`, `packaging/windows/perigee.wxs:27-45`, and `packaging/windows/package-windows.ps1:34,54` bundle `perigee-ui` and `perigee-host` only.
- Confidence: Read
- Impact: Tagged releases are packaged without running workspace/mTLS tests in `release.yml`, and packaged Linux/macOS/Windows installers do not ship the `perigee` CLI needed to manage `known_hosts` or connect headlessly.
- Direction: Add `cargo test --workspace --all-targets --locked` to `release.yml` and bundle the `perigee` CLI binary across Linux, macOS, and Windows packaging scripts.
- Tracked by: UNTRACKED

## Untracked gaps

| # | Gap | Proposed type (bug/feat/task) | Proposed scope | Proposed severity | Proposed title (imperative, ≤80 chars) | From row |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| 1 | SSH pairing via `simply-transfer-link` (Phase 4b) | feat | `perigee_net` | high | Implement SSH pairing via simply-transfer-link (Phase 4b) | Deferred / L-P4-1 / P4-01 / OOB-01,OOB-05,ST-01,ST-05 |
| 2 | Host QUIC port rate limiting, connection caps, handshake timeout | feat | `perigee_host` | medium | Add host QUIC rate limiting, session caps, and handshake timeout | Deferred / L-P4-1 / AUTH-05 / P4-02 |
| 3 | Service packaging non-root user and explicit `PERIGEE_CONFIG_DIR` | bug | `packaging` | high | Configure non-root user and PERIGEE_CONFIG_DIR in OS services | Release surface / L-P4-2 / P4-03 |
| 4 | GUI identity display and `known_hosts`/`trusted_clients` management | feat | `perigee_ui` | high | Add node identity and trust management UI to perigee-ui | Release surface / L-P4-5 / UI-05 / P4-04 |
| 5 | `known_hosts` label canonicalization & duplicate-key eviction bug | bug | `perigee_client` | medium | Canonicalize known_hosts labels and allow multiple targets per host key | G11 / L-P4-6 / P4-05 |
| 6 | Atomic temp-rename creation and DER zeroization for `identity.key` | bug | `perigee_net` | low | Write identity.key atomically via temp file and zeroize read DER buffer | G2 / P4-06 |
| 7 | Fixed `.tmp` race and open handle before `rename` in `TrustedKeys::save` | bug | `perigee_net` | low | Use unique temp file and drop handle before rename in TrustedKeys::save | G10 / P4-07 |
| 8 | Stale auth/crypto docs in `AGENTS.md` and `perigee-wire-and-quic/SKILL.md` | task | `docs` | low | Update AGENTS.md and perigee-wire-and-quic skill for Phase 4 mTLS | Release surface / L-P4-3,L-P4-4,L-P4-7 / P4-08 |
| 9 | Remove `requires_elevation`, `"none (legacy transport)"`, and `HostCredentials` | task | `perigee_protocol` | low | Remove stale requires_elevation field, legacy log text, and HostCredentials | G22 / L-P4-4 / P4-09 |
| 10 | Client logs full `VirtualChannelMessage` payloads at `INFO` level | bug | `perigee_client` | medium | Redact VirtualChannelMessage payload from client INFO logs | CLI-10 / P4-10 |
| 11 | `release.yml` missing `cargo test` and OS installers missing `perigee` CLI | task | `packaging` | medium | Run workspace tests in release.yml and bundle perigee CLI in installers | Release surface / P4-11 |

## Open questions for Claude
1. Should `P4-05` (`known_hosts` default-port `:4433` canonicalization + duplicate-key eviction in `TrustedKeys::add`) and `P4-06`/`P4-07` (`identity.key` atomic rename, `TrustedKeys::save` unique temp file + drop before rename) be fixed now in a small Phase 4 polish step before Phase 4b/5, or queued as pending items for Phase 4b?
2. Should `P4-09` (`HostCredentials` deletion) be folded into existing `TASK-0039` ("Decide fate of NistVault session-token store after Phase 4") rather than tracked as a separate item?
3. Should `P4-03` (hardened non-root service units with explicit `PERIGEE_CONFIG_DIR`) and `P4-11` (`release.yml` tests + `perigee` CLI bundling) be attached to Phase 7 ("Two-App Packaging & Hardened Services") or pulled earlier so operators can run Phase 4 services immediately?
4. Should `crates/perigee-cli/tests/loopback.rs` also gain negative mTLS cases (untrusted client + wrong host key against real `bind_host`/`run_host`), or is coverage in `crates/perigee-net/tests/quic_tests.rs` and `crates/perigee-client/src/connect.rs` sufficient?
