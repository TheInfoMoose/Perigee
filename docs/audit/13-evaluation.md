# 13 Evaluation
Audited HEAD: a7a39c5

Step 3 of the audit workflow. This weighs the validated audit (reports 01–12) against what rFerram is meant to be, and answers the user's question: is it worth pursuing, and if so, what gets reused and what gets rebuilt. It ends with the decisions only the user can make. Finding IDs cite reports 01–10; severities are the adjusted ones from `12-claude-validation.md`.

Estimates in this report are Claude's judgment from the audit, not measurements. They're marked **(estimate)**.

## Verdict

**Go. Rebuild in place, not from a new repo, and delete the stand-ins instead of refactoring them.**

- **The idea holds up.** Direct P2P remote desktop over QUIC, pinned identities, VPN-only reach, a CLI-first backend and a COSMIC frontend is a coherent product with a real gap to fill. Nothing in the audit argues against the niche.
- **The code is mostly scaffolding.** Every requirement is Partial or Missing (`11-traceability.md`, Matrix). The media pipeline, auth, file transfer, OOB, lifecycle and GUI toolkit are stand-ins (CAP-01, ENC-01, AUTH-01, FT-01, OOB-01, LIFE-01, UI-04).
- **But the hard parts are new work either way.** Real capture, encode, decode, input injection and authentication don't exist anywhere in the repo. Starting over doesn't make them cheaper, and keeping the repo doesn't make them more expensive, *as long as the stand-ins are deleted first*. Refactoring a fake capturer into a real one costs more than writing the real one, because the fake one's shape (probe, drop handle, return constant bytes) is wrong.
- **What's worth keeping is the foundation.** The wire format, QUIC plumbing, client session API shape, clipboard core, workspace setup, CI and test habits. That's roughly **2.5–3k of ~11.3k non-test LOC (estimate)**, and it's the part everything else stands on.
- **Why not a fresh repo:** a fresh repo would port the same ~3k lines and rebuild CI, `deny.toml`, packaging skeletons and gates. The saving is a clean history, which you can get anyway by deleting the stand-ins in one visible phase. The difference is small; in-place wins on setup cost.

**The condition:** Phase 2 deletes or quarantines every stand-in before any new feature lands, so nothing fake is left to mislead the next reader (human or agent), the way the README and docstrings mislead today (REPO-04, CAP-04).

## Per-crate verdict
| Crate | Non-test LOC | Verdict | Keep | Delete / rebuild | Key findings |
| :--- | :---: | :--- | :--- | :--- | :--- |
| `rferram-protocol` | 432 | **Keep** | `postcard` framing and bounds; media, input, control, topology and clipboard types | Prune the auth handshake (`ServerHello`/`AuthRequest`/`AuthResponse`) once the link crate owns auth; drop `SimplyTransfer(Vec<u8>)` tunneling; add per-field bounds | PROTO-01, PROTO-03, PROTO-04 |
| `rferram-common` | 363 | **Refactor (small)** | `ConnectionProfile`, `RuntimeEnvironment`; becomes home of the frontend-agnostic profile store | Vault channel-binding helpers and `HostCredentials` (replaced by the link crate) | COM-01, COM-02, UI-02 |
| `rferram-net` | 621 | **Refactor** | `QuicConnectionManager` (datagrams, uni streams, metrics); `VpnSocketFactory` once it fails closed | `PeerCertChannelVerifier`, per-bind cert generation and connection bootstrap (replaced by the link crate); reuse flags | NET-01..10 |
| `rferram-channels` | 1,057 | **Split** | Clipboard core (text/image via `arboard`), once monitor, echo suppression and files are fixed | `oob_ssh.rs` and `snapshot_transfer.rs` (delete; link crate replaces); `elevation.rs`, `passthrough.rs` (delete until in scope) | OOB-01..05, FT-01..04, CLIP-01..04, CH-01..02 |
| `rferram-client` | 1,519 | **Refactor** | `QuicSessionBridge`/`ClientSessionPipeline` shape; loss/ack plumbing; plane-layout math | Absorb the connect flow from `rferram-ui`; real state machine; decoder; presentation sink; clean teardown | CLI-01..09, UI-01 |
| `rferram-server` | 4,809 | **Rebuild internals, keep the seams** | `FrameCapturer` / `VideoEncoder` / input-injector traits; Windows `SendInput` and macOS `CGEventPost` call sites as reference | All capture and encode stubs; Linux input stubs; `HostSessionEngine` lifecycle; handshake; env-only config | CAP-01..03, ENC-01, IN-01..02, AUTH-01..05, LIFE-01..08 |
| `rferram-ui` | 2,512 | **Freeze, then rebuild on a real toolkit** | Nothing structural: it's a launcher shell with misplaced backend logic | Move connect/profile logic out now (it belongs in `rferram-client` / `rferram-common`); rebuild the UI in the GUI phase | UI-01..07 |
| `vendor/libcosmic` | 8,555 | **Retire** | — | Home-built look-alike, not System76's libcosmic. Replace with real upstream libcosmic in the GUI phase | UI-04 |
| `vendor/ST_Rust_Age` | — | **Replace** | — | Stale snapshot of upstream, missing 2A–2G; replace with upstream crates plus `simply-transfer-link` | ST-01..05 |

## Target structure
Libraries stay reusable; binaries stay thin. Arrows show "depends on".

```text
Reusable libraries
  rferram-protocol        wire types + postcard framing (no I/O)
  rferram-net             media transport on an authenticated quinn::Connection:
                          datagrams, uni/bi streams, metrics, VPN socket binding
  rferram-media           host side: capture, encode, input-injection traits + per-OS backends
                          (split out of rferram-server so the daemon is just orchestration)
  rferram-channels        clipboard; file-transfer adapter over simply-transfer-link
  rferram-common          profiles store, config, environment detection
  simply-transfer-link    (ST_Rust_Age repo) SSH control + pairing + pinned QUIC + SharePolicy

Client library
  rferram-client          connect, session state machine, decode, presentation-sink trait,
                          clean teardown -> net, protocol, channels, common, link

Binaries
  rferram-host            daemon + setup subcommands (pair, check permissions, install service)
                          = the lightweight destination app (R9)
  rferram                 full CLI: `rferram connect <profile>` (client) + `rferram host` (embeds host)
  rferram-ui              GUI frontend, later; depends only on rferram-client + common
```

**What `simply-transfer-link` replaces:** `oob_ssh.rs`, the vault channel-binding helpers, `PeerCertChannelVerifier` and per-bind certs, the in-band auth handshake, and `snapshot_transfer.rs`. **What it doesn't provide, and rFerram keeps:** realtime media/input framing, QUIC datagrams, stream priorities, clipboard (`06-file-transfer-st.md`, link fit section 4).

**What rFerram needs from the link crate (feed into ST STEP 4D's design):**
1. Hand back the authenticated `quinn::Connection` (or accept a caller-supplied endpoint) so rFerram runs **one** QUIC endpoint for control, media and bulk transfer.
2. Datagrams enabled on that connection.
3. Listener bound through a caller-supplied socket, so `VpnSocketFactory` can bind it to the VPN interface.

## R8 needs the VPN, not a protocol trick
R8 says the listener answers only its counterpart and silently ignores everything else. Neither protocol rFerram uses can do that on an open interface:
- SSH sends a banner to anyone who opens TCP (OOB-05). The link crate's control plane is SSH, so the same applies.
- QUIC answers any valid Initial with a handshake, even with mutual TLS (NET-06).

**The clean way to meet R8 is the user's own design premise: listen only on the VPN interface, and fail closed if it isn't there.** Traffic that isn't on the VPN never reaches the socket, and WireGuard itself is silent to unauthenticated peers. That makes the fail-open fallback (NET-05) and the `0.0.0.0` default (NET-06, REPO-06) the real R8 blockers, and they're small fixes. Single-packet authorization in front of SSH/QUIC is the alternative, but it's real work for little gain over VPN-only binding. **(Suspected: design reasoning, not verified behavior.)**

## Smallest CLI-first vertical slice
The first slice that proves R2 end to end, on one OS:

1. `rferram-host` listens on a configured interface only, fail closed; clean shutdown on SIGINT/SIGTERM; sessions removed on disconnect.
2. `rferram connect` opens a session to it, authenticated with pinned identities.
3. The host captures the real screen (one display mode), encodes it, and streams it.
4. The client decodes and shows it in a bare window (no GUI shell), and sends mouse and keyboard back.
5. The host injects that input.
6. Disconnect leaves no session, task or socket behind on either side; reconnect works.
7. A CI test runs the real host against the real client on loopback (closes REPO-05, CLI-06).

Clipboard, file transfer, audio, the GUI and the other two OSes all come after this works.

## Suggested phase order
Each phase gets its own approval, per the workflow. Step counts are rough **(estimate)**.

| Phase | Goal | Depends on | Rough size |
| :--- | :--- | :--- | :--- |
| 2 | Hygiene + delete stand-ins: stray root files, unused deps, dead channels/OOB/snapshot code, fake capture/encode, misleading docs; fix fail-open bind, `0.0.0.0` default, reuse flags, accept loop, signals, session cleanup | — | 8–10 steps |
| 3 | CLI client + lifecycle: move connect flow out of `rferram-ui`; `rferram` and `rferram-host` binaries with CLI args; real state machine; clean teardown; real host↔client loopback test | 2 | 8–10 steps |
| ST track | ST_Rust_Age 3A + 4A–4F (`simply-transfer-link`), with rFerram's three needs in 4D | user's hotfix smoke test upstream | ~7 steps (in the ST repo) |
| 4 | Adopt the link crate: pinned auth, pairing, one QUIC endpoint; remove the in-band handshake | 3 + ST track | 5–7 steps |
| 5 | Real media on the first OS: capture, encode, decode, present, input | 4 (3 for a loopback-only dev build) | 10–14 steps (largest) |
| 6 | Clipboard + file transfer over the link crate | 4 | 6–8 steps |
| 7 | Two-app packaging, service units (non-root, VPN-bound) | 3 | 4–5 steps |
| 8 | Other two OSes for media/input | 5 | 8–12 steps each |
| 9 | GUI on real libcosmic | 3, 5 | 10+ steps |

Phases 2–3 and the ST track can run at the same time: they touch different repos.

## Decisions for the user
Each has Claude's recommendation first. The first four change what Phase 2+ looks like; the rest have sensible defaults you can just accept.

1. **Go / no-go and repo.** Recommend: go, rebuild in place, delete stand-ins in Phase 2. Alternative: fresh repo, port ~3k lines.
2. **`simply-transfer-link` timing.** Recommend: co-develop. Run ST 3A + 4A–4F as their own track now, with rFerram's needs in 4D, while rFerram does Phases 2–3. Alternatives: wait for ST to finish on its own; or build interim auth in rFerram (wasted work once the link crate lands).
3. **Display mode first (R6).** Recommend: screen share. On Linux, `xdg-desktop-portal` ScreenCast + PipeWire works on COSMIC, GNOME and KDE and mirrors the session you're already in. Headless needs compositor-specific virtual outputs and is a bigger lift.
4. **First OS for the slice.** Recommend: Linux (COSMIC). It's where you work, and Linux capture and input are the furthest from real today (CAP-01, IN-01), so it removes the most risk. Alternative: Windows first (DXGI + `SendInput` scaffolding exists).
5. **Codec for the first slice.** Adding a codec crate is a dependency change that needs your approval. Recommend: decide at Phase 5 planning, with a measured comparison. Options: software AV1 (`rav1e` + `dav1d`; CPU-heavy at realtime), H.264 (`openh264`; fast, Cisco binary licensing), or hardware (VAAPI / NVENC / VideoToolbox; per-vendor work).
6. **CLI client display.** Recommend: a bare `winit` + `wgpu` window with input capture, no widgets. "Interact" needs a surface to send input from. Alternative: frame dump to files for headless testing (useful in CI either way).
7. **Audio.** Not in R1–R12 but carried by the protocol (AUD-01). Recommend: defer until video and input are real; leave the protocol types.
8. **Elevation and device passthrough.** Recommend: remove both from the protocol until they're in scope (CH-01, CH-02).
9. **VPN ingestion.** Recommend: out for now. Bind to a named interface, fail closed, and leave VPN setup to the user. This is also how R8 gets met (see above).
10. **Lightweight destination app scope (R9).** Recommend: `rferram-host` = daemon + setup subcommands (pair, check permissions, install a non-root service). A GUI onboarding helper (macOS TCC prompts, portal consent) comes with the GUI phase.
11. **GUI toolkit (R10).** Recommend: freeze `rferram-ui` and `vendor/libcosmic` now (only moving backend logic out), and rebuild on real upstream libcosmic in the GUI phase. That's a dependency change, so it needs your approval when the time comes.
12. **`libxkbcommon` (BASE-07).** Recommend: keep linking it and document the dev package. dlopen isn't worth the complexity yet.

## Decisions recorded (user, 2026-11-17)
1. **Go, rebuild in place.** The app is being **renamed to Perigee**.
2. **`simply-transfer-link`: co-develop now.** ST 3A + 4A–4F run as a parallel track, with rFerram's three needs folded into ST STEP 4D.
3. **Display mode: headless is the primary focus.** Release order is **Linux → macOS → Windows**. Target: RustDesk-class out of the gate, with full Wayland and COSMIC Wayland support, taking special care with **cosmic-greeter** (the login screen).
4. **First OS: Linux / COSMIC.**

### What decision 3 changes
- **Headless and the greeter push the host below the user session.** Unattended access before login means the host must reach the greeter's compositor session, which runs as a separate user. The `xdg-desktop-portal` route recommended above needs a consent dialog in a logged-in session, so it can't serve the greeter or unattended access. The likely route is compositor-level protocols (screen copy, virtual pointer/keyboard, or libei) against cosmic-comp, plus a privileged system service that follows the active seat from greeter to user session. **(Suspected: needs a research spike against cosmic-comp and cosmic-greeter before Phase 5 is written.)**
- **Phase 5 grows**, and a research step (what cosmic-comp exposes for headless outputs, capture, input and greeter handoff) comes first.
- **R6 is reordered:** headless and session takeover first, screen share after.

### Follow-up decisions (user, 2026-11-17)
5. **v1 scope, "RustDesk-class but better implemented":**
   - **Unattended access including the login screen** (cosmic-greeter), following the seat into the user session.
   - **Multi-monitor, stable.**
   - **Native scaling:** a headless host shown at the client's resolution (example: a headless gaming rig viewed at the laptop's native resolution), not a scrolling window or a tiny strip. This implies a virtual output sized to the client, resized when the client changes (TopologyMessage `ResizeRequest` becomes real).
   - **Clipboard and file transfer** in v1.
   - **Audio** can follow v1, but should ship close to it.
   - Rationale: RustDesk's Wayland support is "experimental"; Perigee's should be stable.
6. **R1 stays: direct P2P only.** No ID or relay server.
7. **Full rename to Perigee in Phase 2:** crates (`perigee-*`), binaries, packaging, config paths and docs, in its own steps before new crates are added. Renaming the repository folder is the user's step.
8. **Evaluation defaults 5–12 accepted.** With the rename, the light destination app is `perigee-host` (daemon + setup subcommands) and the full CLI is `perigee`. Audio follows item 5.
