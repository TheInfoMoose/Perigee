# 10 Repo Hygiene, Packaging, CI, and Sensitive Data audit
Audited HEAD: 8611388

## Summary
- Ten tracked files at the repository root (`gitignore`, `rferram-workspace-Cargo.toml`, `rferram-ci-workflow.yml`, and seven `rferram-*.rs` files totaling `2,438` lines) are stale scaffolding copies from initial commits `53865b9` and `9623548` that diverge from the live workspace (`Cargo.toml`, `.gitignore`, `crates/*/src/`) or duplicate `.github/workflows/ci.yml` (`L-REPO-1`).
- Every Linux (`.tar.gz`, `.deb`, `.rpm`, `.AppImage`), macOS (`.tar.gz`, `rFerram.app`, `.dmg`), and Windows (`.zip`, `.msi`) package bundles both `rferram-ui` and `rferram-server` together, with no lightweight destination-only package or installer split (`L-REPO-2`, `R9`).
- Service unit configurations across OSes are inconsistent and unsafe (`L-REPO-6`): Linux `rferram-server.service` runs `/usr/bin/rferram-server` as `root` with `AmbientCapabilities=CAP_NET_RAW CAP_SYS_ADMIN`, `RFERRAM_BIND_ADDR=0.0.0.0:4433`, and no systemd sandboxing or display environment; macOS `com.theinfomoose.rferram-server.plist` is copied into `Resources/` without installation; Windows `rferram.wxs` installs `rferram-server.exe` as a plain file with no `<ServiceInstall>` registration.
- CI (`.github/workflows/ci.yml`) and release (`.github/workflows/release.yml`) workflows exercise no authenticated session and no client-server interop against the real `rferram-server` daemon (`L-REPO-5`); `release.yml` omits `cargo test` entirely, and both workflows are currently blocked by two RustSec advisories in `Cargo.lock`.
- Documentation and agent scripts contain broken local developer paths (`<out-of-repo path>` in `AGENTS.md` and `.agents/skills/*`, hardcoded `WORKSPACE_DIR="/srv/devshare/rFerram"` in `verify-crate.sh`, `L-REPO-3`), `README.md` advertises unimplemented capabilities as complete (`L-REPO-4`), and tracked sources/tests/vendor files contain RFC 1918 / CGNAT / public IPv4 literals, `/srv/devshare` and personal user paths, a hardcoded SSH host-key fallback seed, and a deterministic vault fallback key prefix.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| stray files | Partial | 10 stale scaffolding files tracked in repo root (`gitignore`, `rferram-workspace-Cargo.toml`, `rferram-ci-workflow.yml`, 7 `rferram-*.rs` files) alongside untracked `CLAUDE.md` | R11 |
| packaging per OS | Partial | `packaging/linux/package-linux.sh:73-519`, `packaging/macos/package-macos.sh:33-101`, `packaging/windows/package-windows.ps1:47-93` build archives/installers, but Linux fallback AppImage extracts to `/tmp` on every run (`:437-447`), macOS uses ad-hoc `codesign --sign -` (`:82`), and `.deb`/`.rpm` lack service lifecycle scripts | R12 |
| full vs light app split | Missing | `packaging/linux/package-linux.sh:76-77, 103-104, 172-173, 229-230`, `packaging/macos/package-macos.sh:36-37, 53-54`, `packaging/windows/rferram.wxs:28-44` always bundle both `rferram-ui` and `rferram-server` | R9 |
| service unit privileges (systemd, launchd, Windows) | Partial | `packaging/linux/rferram-server.service:6-13` runs as `root` (`CAP_NET_RAW CAP_SYS_ADMIN`) on `0.0.0.0:4433`; `packaging/macos/com.theinfomoose.rferram-server.plist:4-20` has no `UserName` and is never loaded; `packaging/windows/rferram.wxs:42-44` has no `<ServiceInstall>` | R6, R7, R8, R12 |
| CI gates | Partial | `.github/workflows/ci.yml:23-132` runs `cargo audit`, `cargo deny`, `cargo fmt`, `cargo clippy` (Ubuntu only), `cargo test`, and `RFERRAM_SMOKE_TEST=1` bind-only check; no real client-server session test | R2, R3, R12 |
| release gates | Partial | `.github/workflows/release.yml:41-289` runs lint/audit/deny and `RFERRAM_SMOKE_TEST=1` on `rferram-server` and `rferram-ui`, but skips `cargo test` and has no package signing or notarization | R9, R12 |
| docs accuracy | Stub | `README.md:17-53` claims zero-copy GPU capture/encode/render, Opus audio, and snapshot file sync that are stubs in `crates/*`; `AGENTS.md:3, 6` and `.agents/skills/*` reference nonexistent `<out-of-repo path>` paths; `CONTRIBUTING.md:37-40, 57` diverges from workspace rules | R11, R12 |

## README claims
| README.md Claim (`file:line`) | Status | Code Reality & Audit Evidence |
| :--- | :--- | :--- |
| **Zero-Copy Hardware Video Pipeline**: Direct GPU frame buffer capture to hardware encoders (`VideoToolbox`, `DXGI`, `DMABUF / Smithay / PipeWire`) at 60+ FPS (`README.md:11, 17, 47-48`) | Stub | All four capture backends allocate CPU `Vec<u8>` buffers filled with constant byte values (`crates/rferram-server/src/backends/linux_capture.rs:299, 425`, `windows.rs:587`, `macos.rs:383`), and `HostVideoEncoder::encode` truncates raw bytes to 1024/2048 bytes (`crates/rferram-server/src/encoder.rs:83, 153`) with no hardware encoder or `rav1e` dependency (`crates/rferram-server/Cargo.toml:1-46`). |
| **"Bring Your Own Network" (BYON) Transport**: Direct P2P over QUIC (`quinn` + `rustls`) explicitly binding to VPN adapters (`wg0`, `tailscale0`, `utun3`, UniFi) via `socket2` (`README.md:11, 18`) | Partial | `QuicClientTransport` and `QuicServerListener` run over `quinn` (`crates/rferram-net/src/endpoint.rs:35-195`), and `bind_vpn_udp_socket` binds via `socket2` (`crates/rferram-net/src/socket.rs:77-153`), but client TLS verification accepts any certificate without checking signatures (`endpoint.rs:222-255`), server certs are regenerated per bind (`endpoint.rs:120`), and failed adapter resolution silently falls back to `0.0.0.0` (`socket.rs:94-141`). |
| **Native COSMIC & Cross-Platform UI**: Built with `libcosmic` and `cosmic-config`, leveraging `wgpu` (`Vulkan`, `Metal`, `DirectX 12`) for hardware-accelerated rendering and frosted glass aesthetics (`README.md:11, 19, 33, 49`) | Stub | `vendor/libcosmic` (`vendor/libcosmic/Cargo.toml:1-28`) is an in-repo software rasterizer (`taffy` + `tiny-skia`) rather than upstream System76 `libcosmic`; `WgpuRenderPipeline` holds no `wgpu::Device`/`Queue`/`Surface` outside `#[cfg(test)]` (`crates/rferram-client/src/render.rs:265-305, 614, 784`); `rferram-ui` has no remote session viewport (`crates/rferram-ui/src/app.rs:156-184, 845-899`) and never wires `cosmic-config` in `RFerramApp::init` (`app.rs:144-146`). |
| **Low-Latency Input & Datagrams**: Dispatches mouse coordinates, touch gestures, and keyboard scancodes over unordered QUIC datagrams (`README.md:20, 50`) | Partial | `QuicSessionBridge` and `HostDaemon` transport `WireMessage::Input` over QUIC datagrams (`crates/rferram-client/src/bridge.rs:133-141`, `crates/rferram-server/src/daemon.rs:647-679`), and Windows `SendInput` (`windows.rs:1048`) and macOS `CGEventPost` (`macos.rs:728-794`) call OS APIs, but Linux Wayland and X11 input backends only log and increment counters (`crates/rferram-server/src/backends/linux_input.rs:557-612, 864-917`), and `rferram-ui` captures no remote input. |
| **Duplex Audio Synchronization**: Captures host speaker audio (`PipeWire`, `WASAPI`, `CoreAudio`) into 20ms 48kHz Opus chunks with bi-directional microphone passthrough (`README.md:21, 51`) | Stub | No audio library (`cpal` or `opus`) is in `Cargo.toml`; `HostAudioLoopback::capture_20ms_opus_chunk` returns zero-filled 192-byte buffers (`crates/rferram-server/src/audio.rs:28, 59`), client `AudioLoopbackEngine` only tracks counters in memory (`crates/rferram-client/src/audio.rs:62-122`), and neither `daemon.rs` nor `rferram-client` sends or handles `WireMessage::Audio`. |
| **Native Bi-Directional Clipboard Sync**: Uses `arboard` for text, RGBA/PNG images, and file lists with loopback echo suppression (`README.md:22, 37`) | Partial | Text and RGBA image get/set use `arboard` with hash + `AtomicBool` echo suppression (`crates/rferram-channels/src/clipboard.rs:63-176`), but `ClipboardContent::Files` is never read from the OS clipboard and incoming file lists are only stored in `last_received_files` (`clipboard.rs:177-184`), while `spawn_monitor_loop` (`:192-215`) is never called by client, UI, or server. |
| **Snapshot-Backed File Synchronization**: Integrates `ST_Rust_Age` (`simply-transfer-core`, `crypto`, `snapshots`) and embedded `russh` P2P channels via native OS snapshots (`APFS`, `VSS`, `LVM`/`ZFS`/`btrfs`) with streaming SHA-256 verification (`README.md:11, 23, 37, 52`) | Stub | `simply-transfer-core` is unused in `rferram-channels`; `SnapshotTransferHandler::initiate_outbound_transfer` destroys the snapshot immediately (`crates/rferram-channels/src/snapshot_transfer.rs:75`), hardcodes `total_bytes = 50 * 1024 * 1024` (`:96`), and sends only an `ST_INIT:<id>:<path>` string with no file bytes (`:105`); `OobSshServer` (`oob_ssh.rs`) is never started anywhere. |
| **NIST Credential Vault & Session Security**: Ed25519 + `keyring-core` + `simply-transfer-crypto` (`README.md:39`) | Partial | `HostCredentials` and `VaultTokenModel` implement `Zeroize` and redacted `Debug` (`crates/rferram-common/src/models.rs:64-122`), but `SecureStorage` falls back to a world-predictable key (`vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:89, 109-121`), and `rferram-server` grants a full session on an unauthenticated `ClientHello` (`crates/rferram-server/src/daemon.rs:379-399`). |

## Sensitive data sweep
Tracked files (excluding `vendor/libcosmic`, `docs/audit`, `Cargo.lock`, and binary icons) were swept for non-reserved IPv4 literals (outside `127.0.0.0/8`, `0.0.0.0`, `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), email addresses (none found outside binary `.ico` data and `@2x.png` icon filenames), absolute `/home`, `/srv`, `/Users`, and `C:\Users` paths, personal usernames/handles, and hardcoded key/seed/secret strings:

| `file:line` | What | Category | Why it matters |
| :--- | :--- | :--- | :--- |
| `crates/rferram-ui/src/app.rs:210, 1640` | `"10.0.10.15:4433"` | IPv4 (RFC 1918) | Built-in default connection profile and test assertion use a private LAN IP instead of RFC 5737 `192.0.2.0/24`. |
| `crates/rferram-ui/src/app.rs:224, 977` | `"192.168.1.100:4433"`, `"192.168.1.100"` | IPv4 (RFC 1918) | Built-in default connection profile and search test use a private LAN IP instead of RFC 5737 `192.0.2.0/24`. |
| `crates/rferram-ui/src/app.rs:1122, 1139, 1217, 1219` | `"10.42.0.10:4433"`, `"10.77.0.9:4433"` | IPv4 (RFC 1918) | UI modal tests use non-reserved private IPs instead of RFC 5737 `192.0.2.0/24`. |
| `crates/rferram-ui/src/lib.rs:106, 119, 146, 226` | `"10.10.0.12:4433"`, `"100.64.1.25:4433"`, `"10.88.0.5:4433"`, `"10.66.0.8:4433"` | IPv4 (RFC 1918 / RFC 6598 CGNAT) | UI test fixtures use private and Tailscale/CGNAT IPs instead of RFC 5737 documentation ranges. |
| `crates/rferram-ui/src/modal.rs:370, 522, 525` | `"10.20.30.40:4433"`, `"10.55.0.12:4433"` | IPv4 (RFC 1918) | Modal unit tests use non-reserved private IPs instead of RFC 5737 `192.0.2.0/24`. |
| `crates/rferram-net/src/socket.rs:26`, `rferram-net-lib.rs:30` | `` `10.0.0.2` `` | IPv4 (RFC 1918) | Doc comment example uses a private LAN IP instead of RFC 5737 `192.0.2.0/24`. |
| `rferram-ui-app.rs:140, 152` | `"10.0.10.15:4433"`, `"192.168.1.100:4433"` | IPv4 (RFC 1918) | Stale root copy of `app.rs` duplicates non-reserved private IPs. |
| `vendor/ST_Rust_Age/simply-transfer-core/src/firewall.rs:557-558, 570, 578-579, 584, 587, 597, 600, 607` | `"10.88.0.2"`, `"10.88.0.1"`, `"10.88.0.0"` | IPv4 (RFC 1918) | Vendored WireGuard unit tests use non-reserved RFC 1918 addresses alongside `203.0.113.10`. |
| `vendor/ST_Rust_Age/simply-transfer-core/src/registry.rs:237, 247-248, 253` | `"10.0.0.15"`, `"10.99.0.2/24"`, `"1.1.1.1"` | IPv4 (RFC 1918 / Public DNS) | Vendored registry tests use private IPs and public resolver `1.1.1.1`. |
| `vendor/ST_Rust_Age/simply-transfer-crypto/src/keys.rs:248-249, 256, 265-266, 279` | `"10.66.66.2"`, `"10.66.66.1"`, `"192.168.10.0/24"`, `"1.1.1.1"` | IPv4 (RFC 1918 / Public DNS) | Vendored WireGuard config parser tests use non-reserved private IPs and `1.1.1.1`. |
| `AGENTS.md:3, 6, 19` | `/srv/devshare/rFerram`, `<out-of-repo path>` | Host paths & username | Embeds the developer's local Linux username (`<user>`) and shared mount path (`/srv/devshare/rFerram`), and links to a missing directory (`DevDocumentation`). |
| `.agents/skills/rferram-quality-gate/scripts/verify-crate.sh:4` | `WORKSPACE_DIR="/srv/devshare/rFerram"` | Host path | Hardcodes the local `/srv/devshare/rFerram` mount path so the script fails on any other checkout path or CI runner. |
| `.agents/skills/rferram-quality-gate/SKILL.md:15, 18, 24, 29`, `.agents/skills/rferram-st-channels/SKILL.md:42`, `.agents/skills/rferram-wire-and-quic/SKILL.md:46` | `/srv/devshare/rFerram/...` | Host path | Hardcodes the local `/srv/devshare/rFerram` workspace path across agent skill instructions. |
| `.agents/skills/rferram-capture-and-compositor/SKILL.md:17-18, 22, 45-46` | `<out-of-repo path>`, `/srv/devshare/rFerram/...` | Host paths & username | Embeds local user home paths and username (`<user>`) pointing to external zip archives absent from the repository. |
| `.agents/skills/rferram-cosmic-iced-ui/SKILL.md:18-20, 22-24, 75-76` | `<out-of-repo path>`, `/srv/devshare/rFerram/...` | Host paths & username | Embeds local user home paths and username (`<user>`) pointing to missing external documentation and zip archives. |
| `crates/rferram-common/src/lib.rs:117`, `crates/rferram-ui/src/lib.rs:111`, `crates/rferram-channels/src/lib.rs:114, 120, 223-224`, `crates/rferram-protocol/tests/wire_tests.rs:340-341`, `crates/rferram-server/src/daemon.rs:1048, 1078` | `"/srv/devshare"`, `"/srv/devshare/report.pdf"`, `"/srv/devshare/alpha.tar.zst"`, `"/srv/devshare/beta.sig"`, `"/srv/devshare/archive.tar.zst"`, `"/home/user/Documents/report.pdf"` | Host paths | Unit and integration test fixtures across 5 workspace crates hardcode the developer's local `/srv/devshare` mount and `/home/user` paths. |
| `vendor/ST_Rust_Age/simply-transfer-core/src/engine.rs:1143-1210` | `"/Users/moose_dev/Dev..."`, `r"C:\Users\moose_dev\Dev..."`, `r"C:\Users\Zak\tools..."`, `"/home/zak/Remote/tools"` | Host paths & usernames | Vendored `ST_Rust_Age` test fixtures embed personal usernames (`moose_dev`, `Zak`, `zak`) and OS user paths. |
| `vendor/ST_Rust_Age/simply-transfer-snapshots/src/lib.rs:296, 330` | `"/home/user/docs"`, `r"C:\Users\Admin\Documents"` | Host paths & usernames | Vendored snapshot volume-resolution tests use non-reserved user directory paths. |
| `crates/rferram-common/src/lib.rs:130-131, 141` | `username: "admin"`, `secret: "super-secret-password"`, `session_token: "secret-jwt-or-opaque-token"` | Username & test credentials | Test fixture uses `"admin"` rather than reserved placeholder `"testuser"`. |
| `crates/rferram-channels/src/oob_ssh.rs:72-73` | `b"rferram-oob-ssh-fallback-seed-v1"` | Hardcoded cryptographic seed | Production fallback deterministically derives the OOB SSH server's Ed25519 host key from a public 32-byte ASCII constant whenever the vault fails. |
| `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:89` | `b"simply-transfer-local-key-v1:"` | Hardcoded key derivation prefix | Production fallback derives the AES-256-GCM vault key solely from `SHA-256("simply-transfer-local-key-v1:" + service + $USER)` with zero secret entropy. |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-REPO-1: ten tracked root files are stale copies (list each with its live counterpart and git first-commit). | Confirmed | `gitignore` (`9623548`), `rferram-workspace-Cargo.toml` (`53865b9`), `rferram-ci-workflow.yml` (`53865b9`), and 7 `rferram-*.rs` files (`53865b9`) are tracked in the repo root alongside their live counterparts in `.gitignore`, `Cargo.toml`, `.github/workflows/ci.yml`, and `crates/*/src/`. | REPO-01 |
| L-REPO-2: every package bundles both `rferram-ui` and `rferram-server`; no lightweight destination package (R9). | Confirmed | `packaging/linux/package-linux.sh:76-77, 103-104, 172-173, 229-230`, `packaging/macos/package-macos.sh:36-37, 53-54`, `packaging/windows/package-windows.ps1:53-54`, and `packaging/windows/rferram.wxs:28-44` unconditionally package both binaries together. | REPO-02 |
| L-REPO-3: `AGENTS.md` cites `DevDocumentation` (missing); `verify-crate.sh` hardcodes `WORKSPACE_DIR`. | Confirmed | `AGENTS.md:3, 6` and `.agents/skills/rferram-{capture-and-compositor,cosmic-iced-ui}/SKILL.md` reference nonexistent `<out-of-repo path>` paths; `.agents/skills/rferram-quality-gate/scripts/verify-crate.sh:4` hardcodes `WORKSPACE_DIR="/srv/devshare/rFerram"`. | REPO-03 |
| L-REPO-4: README claims (zero-copy hardware pipeline, Opus, VAAPI/NVENC, snapshot file sync) exceed the implementation. | Confirmed | `README.md:17-24, 45-53` and `packaging/linux/com.theinfomoose.rFerram.metainfo.xml:9-13` describe hardware capture/encode/render, Opus audio, and snapshot file sync as implemented features despite stubbed backends across `rferram-server`, `rferram-client`, `rferram-channels`, and `rferram-ui`. | REPO-04 |
| L-REPO-5: CI tests no authenticated session and no server-client interop with the real daemon. | Confirmed | `.github/workflows/ci.yml:125-132` runs unit/mock tests (`e2e_loopback.rs` uses an in-test mock host) plus `RFERRAM_SMOKE_TEST=1` (`crates/rferram-server/src/main.rs:32-37`), which exits before `listener.accept()`; `.github/workflows/release.yml:38-289` runs no `cargo test` at all. | REPO-05 |
| L-REPO-6: which user and privileges the systemd unit / launchd plist / MSI service run with. | Confirmed | `packaging/linux/rferram-server.service:6-13` runs as `root` with `AmbientCapabilities=CAP_NET_RAW CAP_SYS_ADMIN` and `RFERRAM_BIND_ADDR=0.0.0.0:4433`; `packaging/macos/com.theinfomoose.rferram-server.plist:4-20` omits `UserName`, binds `0.0.0.0:4433`, and is never installed; `packaging/windows/rferram.wxs:42-44` omits `<ServiceInstall>` entirely. | REPO-06 |

## Findings

### REPO-01: Ten stale scaffolding files (`2,438` LOC) are tracked at the repository root
- Severity: Low
- Category: hygiene
- Requirement: R11
- Evidence: `rferram-workspace-Cargo.toml:1-19`, `rferram-unit-tests.rs:38-43, 155-169`
  ```toml
  [workspace]
  resolver = "2"
  ```
- Confidence: Ran (`git ls-files | grep -v '/'` and `git log --reverse --format="%h %ad %s"`)
- Impact: Ten root-level files committed during initial scaffolding carry obsolete designs (`edition = "2021"`, `bincode`, unvendored git deps, broken unit tests) and pollute repository searches:
  1. `gitignore` (`23` lines, first commit `9623548`) -> live `.gitignore` (`25` lines, first commit `789f472`).
  2. `rferram-workspace-Cargo.toml` (`82` lines, first commit `53865b9`) -> live `Cargo.toml` (`152` lines, first commit `789f472`).
  3. `rferram-ci-workflow.yml` (`132` lines, first commit `53865b9`) -> live `.github/workflows/ci.yml` (`132` lines, byte-identical).
  4. `rferram-protocol-lib.rs` (`304` lines, first commit `53865b9`) -> live `crates/rferram-protocol/src/lib.rs`.
  5. `rferram-net-lib.rs` (`370` lines, first commit `53865b9`) -> live `crates/rferram-net/src/{lib,endpoint,manager,socket,metrics}.rs`.
  6. `rferram-channels-lib.rs` (`338` lines, first commit `53865b9`) -> live `crates/rferram-channels/src/{lib,clipboard,snapshot_transfer,oob_ssh,passthrough,elevation}.rs`.
  7. `rferram-client-session.rs` (`273` lines, first commit `53865b9`) -> live `crates/rferram-client/src/{lib,session,bridge,render,audio}.rs`.
  8. `rferram-server-engine.rs` (`469` lines, first commit `53865b9`) -> live `crates/rferram-server/src/{lib,daemon,encoder,audio,backends/*}.rs`.
  9. `rferram-ui-app.rs` (`229` lines, first commit `53865b9`) -> live `crates/rferram-ui/src/{lib,app,modal,drawers,session_controller,shader}.rs`.
  10. `rferram-unit-tests.rs` (`218` lines, first commit `53865b9`) -> live `crates/*/tests/*.rs` and inline `#[cfg(test)]` modules.
- Direction: Remove all ten stale root scaffolding files from git tracking and add `CLAUDE.md` to `.gitignore` if it is local workflow state.

### REPO-02: All OS packaging scripts bundle `rferram-ui` and `rferram-server` into a single artifact with no lightweight destination package
- Severity: High
- Category: structure
- Requirement: R9
- Evidence: `packaging/linux/package-linux.sh:103-104`, `packaging/macos/package-macos.sh:53-54`, `packaging/windows/rferram.wxs:28-44`
  ```bash
  install -m 0755 "${BIN_DIR}/rferram-ui" "${DEB_ROOT}/usr/bin/rferram-ui"
  install -m 0755 "${BIN_DIR}/rferram-server" "${DEB_ROOT}/usr/bin/rferram-server"
  ```
- Confidence: Read
- Impact: Every generated package (`.tar.gz`, `.deb`, `.rpm`, `.AppImage`, `rFerram.app`/`.dmg`, `.zip`, `.msi`) requires and ships the full viewer GUI alongside the host daemon, failing R9's requirement for a separate lightweight destination application (permissions/config setup + daemon).
- Direction: Split packaging targets and crate binaries into the full source+destination suite and a lightweight destination-only package containing only the host daemon and setup utility.

### REPO-03: `AGENTS.md`, workspace skills, and `verify-crate.sh` hardcode `/srv/devshare/rFerram` and nonexistent `<out-of-repo path>` paths
- Severity: Low
- Category: hygiene
- Requirement: R11
- Evidence: `AGENTS.md:3-6`, `.agents/skills/rferram-quality-gate/scripts/verify-crate.sh:4-7`
  ```bash
  WORKSPACE_DIR="/srv/devshare/rFerram"
  TARGET_CRATE="${1:---all}"
  cd "${WORKSPACE_DIR}"
  ```
- Confidence: Ran (`ls -la DevDocumentation`)
- Impact: `verify-crate.sh` fails if the repository is cloned outside `/srv/devshare/rFerram`, and `AGENTS.md:6` plus `.agents/skills/rferram-{capture-and-compositor,cosmic-iced-ui}/SKILL.md` instruct agents to read files under `<out-of-repo path>` that do not exist on disk or in git.
- Direction: Resolve `WORKSPACE_DIR` dynamically via `git rev-parse --show-toplevel` (or `BASH_SOURCE[0]`) in `verify-crate.sh` and replace host-specific `/home/...` and `/srv/devshare/...` paths in `AGENTS.md` and `.agents/skills/` with repository-relative paths.

### REPO-04: `README.md` and Linux AppStream metadata advertise unimplemented capture, encoding, rendering, audio, and file-transfer capabilities as complete
- Severity: Medium
- Category: hygiene
- Requirement: R11
- Evidence: `README.md:17-24`, `packaging/linux/com.theinfomoose.rFerram.metainfo.xml:9-13`
  ```markdown
  * **Zero-Copy Hardware Video Pipeline**: Direct GPU frame buffer capture to hardware encoders (Apple Silicon / VideoToolbox on macOS, DXGI on Windows, DMABUF / Smithay / PipeWire on Linux) streaming at 60+ FPS.
  ```
- Confidence: Read
- Impact: Users and evaluators reading `README.md` or package metadata are told the project provides hardware zero-copy capture/encode/render, Opus audio, and snapshot-backed file transfer when all of those subsystems are currently stubs.
- Direction: Rewrite `README.md` and `com.theinfomoose.rFerram.metainfo.xml` to accurately reflect implemented vs planned capabilities once Phase 1 evaluation decisions are finalized.

### REPO-05: CI and release workflows never test an authenticated session or real daemon-to-client interop, and `release.yml` skips `cargo test`
- Severity: High
- Category: gap
- Requirement: R2
- Evidence: `.github/workflows/ci.yml:125-132`, `.github/workflows/release.yml:75-86, 191-205`
  ```yaml
        - name: Execute Host Daemon Smoke Test
          env:
            RFERRAM_SMOKE_TEST: "1"
            RFERRAM_BIND_ADDR: "127.0.0.1:0"
          run: cargo run -p rferram-server --locked
  ```
- Confidence: Read
- Impact: `RFERRAM_SMOKE_TEST=1` exits immediately after binding the UDP socket (`crates/rferram-server/src/main.rs:32-37`) without accepting a connection, `e2e_loopback.rs` talks only to an in-test mock server, `release.yml` builds and publishes releases without running `cargo test`, and both workflows fail on `RUSTSEC-2026-0495` and `RUSTSEC-2026-0486` (`01-baseline.md` `BASE-05`).
- Direction: Add an end-to-end integration test in CI and `release.yml` that spawns the real `rferram-server` daemon and connects a headless `rferram-client` over authenticated QUIC, and run `cargo test --workspace --all-targets --locked` in `release.yml`.

### REPO-06: OS service definitions run `rferram-server` as unsandboxed `root` on `0.0.0.0:4433` (Linux/macOS) or fail to register a service at all (macOS/Windows)
- Severity: High
- Category: security
- Requirement: R6
- Evidence: `packaging/linux/rferram-server.service:6-13`, `packaging/macos/com.theinfomoose.rferram-server.plist:8-19`, `packaging/windows/rferram.wxs:42-44`
  ```ini
  Environment="RFERRAM_BIND_ADDR=0.0.0.0:4433"
  ExecStart=/usr/bin/rferram-server
  AmbientCapabilities=CAP_NET_RAW CAP_SYS_ADMIN
  ```
- Confidence: Read
- Impact: On Linux, `rferram-server` runs as `root` with `CAP_NET_RAW` and `CAP_SYS_ADMIN` listening on all interfaces (`0.0.0.0:4433`) while authentication is bypassed (`AUTH-01`) and without user display session variables (`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`) or systemd hardening; on macOS, the `.plist` is placed in `Resources/` without being loaded; on Windows, `rferram.wxs` installs `rferram-server.exe` without `<ServiceInstall>` or `<ServiceControl>`.
- Direction: Run the Linux service under a least-privilege user/session unit with explicit systemd sandboxing, bind only to configured interfaces, and add proper launchd/Windows Service registration and lifecycle hooks.

### REPO-07: Linux fallback `.AppImage` builder compiles a custom C wrapper that extracts a `.tar.gz` into `/tmp` on every execution
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `packaging/linux/package-linux.sh:244-253, 432-448, 484`
  ```c
  snprintf(work_dir, sizeof(work_dir), "%s/appimage_rferram_XXXXXX", tmp_base);
  if (!mkdtemp(work_dir)) {
  ```
- Confidence: Read
- Impact: Because `.github/workflows/release.yml:138-155` does not install `appimagetool`, CI always takes the `gcc` fallback branch in `package-linux.sh:244`, producing a non-FUSE pseudo-AppImage that forks `tar -xzf` into `/tmp/appimage_rferram_XXXXXX` on every launch and leaves residual `/tmp` trees behind if the wrapper receives `SIGKILL` or crashes before `remove_tree` (`:484`).
- Direction: Build standard squashfs Type-2 AppImages with `appimagetool` in CI or drop `.AppImage` in favor of native `.deb`/`.rpm`/`.tar.gz` bundles.

### SENS-01: Production defaults, unit tests, and vendored tests contain RFC 1918, CGNAT, and public IPv4 addresses outside RFC 5737 documentation ranges
- Severity: Low
- Category: sensitive-data
- Requirement: R11
- Evidence: `crates/rferram-ui/src/app.rs:210, 224`, `crates/rferram-ui/src/lib.rs:106, 119`
  ```rust
  host_address: String::from("10.0.10.15:4433"),
  ```
- Confidence: Ran (`git grep` + Python `ipaddress` filter across tracked files)
- Impact: Forty-one occurrences across `crates/rferram-ui/src/{app,lib,modal}.rs`, `crates/rferram-net/src/socket.rs:26`, stale `rferram-{net-lib,ui-app}.rs`, and `vendor/ST_Rust_Age/simply-transfer-{core,crypto}` use RFC 1918 (`10.0.0.0/8`, `192.168.0.0/16`), RFC 6598 (`100.64.1.25`), and public (`1.1.1.1`) IPv4 literals instead of reserved test ranges (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`).
- Direction: Replace all non-loopback example and fixture IPv4 literals with addresses from `192.0.2.0/24` (e.g., `192.0.2.10:4433`).

### SENS-02: Source tests, docs, skills, and vendored crates embed local `/srv/devshare` and `/home/<user>` paths and personal usernames
- Severity: Low
- Category: sensitive-data
- Requirement: R11
- Evidence: `crates/rferram-common/src/lib.rs:117, 130`, `vendor/ST_Rust_Age/simply-transfer-core/src/engine.rs:1178-1192`
  ```rust
  folder_shares: vec!["/srv/devshare".to_string()],
  ```
- Confidence: Ran (`git grep` across tracked files)
- Impact: Tracked files across five workspace crates (`rferram-common`, `rferram-protocol`, `rferram-channels`, `rferram-server`, `rferram-ui`), `AGENTS.md`, `.agents/skills/*`, and `vendor/ST_Rust_Age` leak internal development paths (`/srv/devshare/...`, `/home/<user>/...`, `/Users/moose_dev/...`, `C:\Users\Zak\...`, `/home/zak/...`) and usernames (`<user>`, `moose_dev`, `Zak`, `admin`).
- Direction: Replace all personal usernames and local host paths in tests, docs, and fixtures with `testuser` and generic relative or synthetic paths.

## Open questions for Claude
1. For R9 (full app vs lightweight destination app), should the lightweight destination artifact be a headless CLI + daemon bundle (`rferram-server` + setup subcommand) or should it also include a minimal graphical permission-onboarding helper for macOS TCC (`ScreenCaptureKit`/`Accessibility`) and Wayland portal consent?
2. On Linux, `rferram-server` needs access to the active desktop compositor session (`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` or PipeWire/portal D-Bus) for screen share, which a root `multi-user.target` systemd unit cannot see directly: should the Linux daemon be packaged as a `systemd --user` service for screen-share mode and only use a privileged helper/udev rule when virtual input or headless display creation requires it?

## Claude validation correction (2026-11-17)

Two rows in the "Sensitive data sweep" table quote string literals that do not exist anywhere in the tree (`grep -rn "fallback-seed\|local-key-v1" crates vendor/ST_Rust_Age` returns nothing). The underlying defects are real; only the quoted values are wrong. Gemini's text above is left as written.

| Row as written | Correct evidence (Confidence: Read) |
| :--- | :--- |
| `crates/rferram-channels/src/oob_ssh.rs:72-73` — `b"rferram-oob-ssh-fallback-seed-v1"` | The fallback seed is computed, not a string: `std::array::from_fn(\|i\| (i as u8).wrapping_mul(37).wrapping_add(11))` at `oob_ssh.rs:72-73` (OOB-03). Still public and deterministic. |
| `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:89` — `b"simply-transfer-local-key-v1:"` | The key-derivation prefix is `b"SimplyTransfer:NIST-SP-800-57:VaultKey:"` at `lib.rs:115`, hashed with `service` and `$USER` (COM-01). `:89` is the `temp_dir()` fallback directory, not the key. |

Also confirmed while validating: the systemd unit has no `User=`, so it runs as root (REPO-06); `rferram-ci-workflow.yml` is byte-identical to `.github/workflows/ci.yml` (`cmp`); `release.yml` contains no `cargo test`; `rferram.wxs` has no `<ServiceInstall>`. The sweep excluded `vendor/libcosmic` per the step spec, but 09-ui-coupling (UI-04, UI-09) shows that tree is in-repo code with its own non-reserved IPs (`raster.rs:2520, 2604, 2696`, `runner.rs:1717-1733`); treat those as part of SENS-01.

Redaction (2026-11-21): personal home paths quoted above were replaced with relative paths or placeholders.
