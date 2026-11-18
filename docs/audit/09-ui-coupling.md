# 09 UI Coupling and Vendored libcosmic audit
Audited HEAD: 3cbb929

## Summary
- `rferram-ui` owns core client backend orchestration in `crates/rferram-ui/src/session_controller.rs:330-604` (`UiSessionController::connect_profile_inner`), directly wiring `rferram_net::QuicClientTransport`, `rferram_client::QuicSessionBridge`, `rferram_client::WgpuRenderPipeline`, and `rferram_channels::VirtualChannelManager` while binding async session lifecycle to `cosmic::app::Task<Message>` (`:146-151, 286-297`).
- Connection profile persistence is coupled exclusively to `cosmic-config` (`crates/rferram-ui/src/lib.rs:33-50`, `crates/rferram-ui/src/app.rs:259-325`), and `RFerramApp::init` (`app.rs:144-146`) never calls `with_persistence_config`, leaving `persistence_config: None` in the production `rferram-ui` binary (`crates/rferram-ui/src/main.rs:49, 65`) so profiles are never loaded or saved outside unit tests.
- `rferram-ui` has no remote desktop session viewport or input forwarding (`app.rs:156-184, 845-899`): connecting a profile stores frames in `ActiveUiSession::latest_frame` (`session_controller.rs:430`) and `latest_aligned_plane0` (`:426`), which are never read outside tests (`:1080`), while the UI remains on the connection card grid.
- `vendor/libcosmic` (`8,555` LOC across 7 files) is not an upstream `pop-os/libcosmic` checkout: it was written from scratch in-repo (`789f472`, `d5556e8`, `8c96e8d`, `19ae4ee`, `f21aff5`) as a CPU software rasterizer (`taffy` + `tiny-skia` + `winit` + `wgpu`) with hardcoded `rferram-*` container class rules (`vendor/libcosmic/src/layout.rs:18-78`) and a 5x7 ASCII-only bitmap font (`vendor/libcosmic/src/raster.rs:28-35, 2245-2474`).
- Multiple UI surfaces are non-functional facades: Settings and Help drawers render only a section header label and a `"Close"` button (`crates/rferram-ui/src/drawers.rs:107-124, 153-168`), `Devices | Audio` modal checkboxes dispatch a no-op tab-selection message (`crates/rferram-ui/src/modal.rs:262-271`), `ConfigureConnection` + `"Add"` appends a duplicate card (`app.rs:431-466, 497-524`), and `Import`/`Export` only copy profiles within an in-memory `Vec` (`app.rs:356-394, 543-560`).

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Profile management | Partial | `crates/rferram-ui/src/app.rs:199-325, 356-394, 431-579` and `crates/rferram-ui/src/lib.rs:33-50`; in-memory add/duplicate/delete/favorite/search and `cosmic-config` roundtrips pass `cargo test -p rferram-ui --locked` (26 passed), but `RFerramApp::init` (`app.rs:144-146`) never attaches `persistence_config` in production (`main.rs:49`), `ConfigureConnection` + `SaveConnectionProfile` (`:431-466, 497-524`) creates a duplicate UUID card instead of updating in place, and `Import`/`Export` (`:356-394, 543-560`) only mutate an in-memory `exported_profiles` `Vec` | R2, R9, R10 |
| Connect flow | Partial | `crates/rferram-ui/src/session_controller.rs:286-604` and `crates/rferram-ui/src/app.rs:467-496`; `connect_profile_inner` binds `QuicClientTransport`, spawns `QuicSessionBridge`, and starts video/bi-stream/RTT tasks, but hardcodes `"rferram.host"`, `enable_clipboard: true`, and `VideoCodec::AV1` (`:353, 389-391`), sends only unauthenticated `ClientHello`, fabricates `"Connected"` / `60.0` FPS / `0.5 ms` RTT after a `200 ms` wait (`:506-553`), and offers no UI disconnect button (`app.rs:565` only disconnects on delete) | R1, R2, R3, R7, R10 |
| Session view | Missing | `crates/rferram-ui/src/app.rs:156-184, 845-899` and `crates/rferram-ui/src/session_controller.rs:198-205, 411-447`; `RFerramApp::view` only renders the launcher grid/modals/drawers with an empty `container("rferram-card-preview", row(0, vec![]))` (`app.rs:872`), never reads `ActiveUiSession::latest_frame` or `latest_aligned_plane0` outside tests (`session_controller.rs:1080`), never displays `active_fps`/`active_rtt_ms`/`session_statuses` in the view tree, and never forwards window mouse/keyboard input to `ActiveUiSession::send_input` | R6, R10 |
| Settings | Stub | `crates/rferram-ui/src/drawers.rs:86-174` and `crates/rferram-ui/src/modal.rs:239-313`; `view_settings_drawer` (`drawers.rs:107-124`) and `view_help_drawer` (`:153-168`) render only `"Settings: <section>"` / `"Help: <section>"` and a `"Close"` button with zero interactive controls, while modal `Display` (`modal.rs:247-254`) and `Credentials` (`:310`) dropdowns have no `on_select` callbacks and `Devices | Audio` checkboxes (`:262-271`) emit `Message::SelectAddModalTab(DevicesAudio)` instead of toggling state | R9, R10 |
| COSMIC integration (`cosmic-config`, theming) | Stub | `vendor/libcosmic/Cargo.toml:1-30`, `vendor/libcosmic/cosmic-config/src/lib.rs:1-98`, and `crates/rferram-ui/src/shader.rs:10-27`; `vendor/libcosmic` is an in-repo software rasterizer with no upstream `pop-os/libcosmic`, `iced`, `cosmic-theme`, `cosmic-text`, or D-Bus theme watcher (`dbus-config = []` at `Cargo.toml:18`), `cosmic-config` writes custom JSON (`cosmic-config/src/lib.rs:93`) instead of COSMIC RON, and `FROSTED_GLASS_SHADER_WGSL` (`shader.rs:10-27`) is never bound to a GPU pipeline | R10, R11 |
| Non-COSMIC desktop support | Partial | `vendor/libcosmic/Cargo.toml:25-30`, `vendor/libcosmic/src/runner.rs:462-568, 852-863, 1272-1445`, and `vendor/libcosmic/src/raster.rs:28-35, 2245-2474`; runs as a standalone `winit` + `wgpu` window without requiring a COSMIC session, but CPU-rasterizes the full window via `tiny-skia` on every redraw, supports only 5x7 printable ASCII (`0x20..=0x7E`), and cycles dropdowns via `(idx + 1) % options.len()` on click (`runner.rs:860`) instead of opening a menu | R10, R12 |

## Logic to extract
| Non-presentation responsibility in `rferram-ui` | Current `file:line` range | Target backend crate |
| :--- | :--- | :--- |
| VPN adapter enum & interface resolution (`VpnAdapterBinding`, `to_interface_option`, `from_interface_option`, `to_vpn_interface_binding`, `resolve_vpn_interface_binding`) | `crates/rferram-ui/src/modal.rs:28-73`, `crates/rferram-ui/src/session_controller.rs:48-79` | `rferram-common` (model) / `rferram-net` (adapter-to-`VpnInterfaceBinding` resolution) |
| Host address parsing & DNS/socket resolution (`DEFAULT_RFERRAM_PORT`, `resolve_profile_socket_addr` via `ToSocketAddrs`) | `crates/rferram-ui/src/session_controller.rs:37, 81-111` | `rferram-net` or `rferram-client` (async endpoint target resolution) |
| Background Tokio runtime holder (`SessionRuntimeHolder::new`, `handle`, `Drop` calling `shutdown_background`) | `crates/rferram-ui/src/session_controller.rs:213-249` | `rferram-client` (or shared `tokio::runtime::Handle` injected by binary entrypoint) |
| End-to-end session connect orchestration (`ActiveUiSession`, `UiSessionController::connect_profile`, `connect_profile_inner`, `disconnect_profile`, 200 ms handshake wait, `QuicClientTransport` + `QuicSessionBridge` + `WgpuRenderPipeline` + `VirtualChannelManager` wiring) | `crates/rferram-ui/src/session_controller.rs:154-211, 302-604, 804-818` | `rferram-client` (unified client session controller usable by both CLI and GUI frontends) |
| Bi-directional QUIC virtual channel listener loop (`accept_bi`, `read_to_end`, `decode_length_prefixed`, `VirtualChannelManager::route_channel_message`) | `crates/rferram-ui/src/session_controller.rs:190-195, 449-483` | `rferram-client` (`QuicSessionBridge`) + `rferram-channels` (`VirtualChannelManager`) |
| Video frame consumer & 256-byte row alignment packing loop (`video_rx.recv()`, `WgpuRenderPipeline::reconfigure`, `pack_plane_to_wgpu_alignment`) | `crates/rferram-ui/src/session_controller.rs:365-376, 403-447` | `rferram-client` (`render` / `session` pipeline) |
| Inbound `WireMessage` dispatch & telemetry aggregation (`SessionTelemetrySnapshot`, `refresh_telemetry`, `handle_wire_message`, RTT monitor loop) | `crates/rferram-ui/src/session_controller.rs:113-123, 362-363, 485-503, 606-802` | `rferram-client` (`ClientSessionPipeline` metrics and state machine) |
| Connection profile persistence, favorite IDs, and import/export (`open_ui_config`, `save_connection_profiles`, `load_connection_profiles`, `save_profiles_to_config`, `load_profiles_from_config`, `ImportConnections`, `ExportAllConnections`) | `crates/rferram-ui/src/lib.rs:24-50`, `crates/rferram-ui/src/app.rs:259-325, 356-394, 543-560` | `rferram-common` (frontend-agnostic profile store shared by CLI and GUI) |

## libcosmic local changes
No pristine upstream `pop-os/libcosmic` revision is recorded anywhere in git history, `Cargo.toml`, `Cargo.lock`, or source comments. Although root `Cargo.toml:140-142` declares `[patch."https://github.com/pop-os/libcosmic"]` (which Cargo ignores because `[workspace.dependencies]` at `Cargo.toml:41-42` already uses `path = "vendor/libcosmic"`), `vendor/libcosmic` contains **zero** upstream System76 `libcosmic` code (`iced`, `cosmic-text`, `cosmic-theme`, `freedesktop-icons`, `sctk`, or `ron`). Every file under `vendor/libcosmic` (`8,555` total lines across 7 files) was authored in-repo across 5 commits:

| Commit | Date | Files touched under `vendor/libcosmic` | Approx. size | Purpose of change |
| :--- | :--- | :--- | :--- | :--- |
| `789f472` | 2026-10-25 | `Cargo.toml`, `cosmic-config/Cargo.toml`, `cosmic-config/src/lib.rs`, `src/lib.rs` | `+594 / -0` lines (4 files) | Initial in-repo creation of a mock `libcosmic` (`cosmic`) widget tree (`WidgetNode`, `Element`, `Application`, `Task`) and a 98-line JSON-backed `cosmic-config` crate. |
| `d5556e8` | 2026-10-27 | `Cargo.toml` | `+1 / -4` lines (1 file) | Removed unused Linux `zbus` dependency and narrowed `winit` features (`default-features = false`, `rwh_06`, `x11`, `wayland`, `wayland-dlopen`). |
| `8c96e8d` | 2026-10-29 | `src/layout.rs` (NEW), `src/lib.rs`, `src/raster.rs` (NEW), `src/runner.rs` (NEW) | `+5,131 / -12` lines (4 files) | Added `taffy` layout engine (`layout.rs`), `tiny-skia` software rasterizer with hardcoded 5x7 ASCII bitmap font and frosted glass tint (`raster.rs`), and `winit` + `wgpu` full-screen quad presenter (`runner.rs`). |
| `19ae4ee` | 2026-10-29 | `src/lib.rs`, `src/raster.rs`, `src/runner.rs` | `+128 / -2` lines (3 files) | Added `rasterize_themed_app_icon` in `raster.rs`, `Settings::window_icon` in `lib.rs`, and `WindowAttributes::with_window_icon` in `runner.rs`. |
| `f21aff5` | 2026-10-29 | `src/layout.rs`, `src/lib.rs`, `src/raster.rs`, `src/runner.rs` | `+2,912 / -152` lines (4 files) | Added `ButtonStyle`/`TogglerStyle` variants, `icon::from_name` builder, 22 hardcoded vector icons (`SYMBOLIC_ICON_NAMES`), and custom `rferram-*` container class layout and painting rules for headers, portrait cards, popup menus, modals, and split drawers. |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-UI-1: `connect_profile_inner` owns the full connect flow and `spawn_connect_task`/`into_message` tie it to cosmic types. | Confirmed | `crates/rferram-ui/src/session_controller.rs:146-151, 286-297, 330-604`; `connect_profile_inner` (`:330-604`) directly instantiates `QuicClientTransport`, `WgpuRenderPipeline`, `VirtualChannelManager`, `QuicSessionBridge`, and 3 `tokio::spawn` loops, while `spawn_connect_task` (`:286-297`) wraps execution in `cosmic::app::Task::perform` and `SessionConnectOutcome::into_message` (`:146-151`) returns `crate::Message::SessionStatusChanged` | UI-01 |
| L-UI-2: profiles persist only via `cosmic_config` (`app.rs:259-310`). | Confirmed | `crates/rferram-ui/src/lib.rs:33-50` and `crates/rferram-ui/src/app.rs:78, 144-146, 244, 259-325`; profile persistence is implemented exclusively via `cosmic_config::Config` in `rferram-ui`, and `RFerramApp::init` (`app.rs:144-146`) never calls `with_persistence_config`, leaving `persistence_config: None` in the production binary (`main.rs:49`) | UI-02 |
| L-UI-3: clipboard hard-coded on, AV1 forced (`session_controller.rs:380-392`); 200 ms handshake wait (`:499-525`). | Confirmed | `crates/rferram-ui/src/session_controller.rs:380-392, 505-553`; `SessionConfig` hardcodes `enable_clipboard: true` (`:389`), `max_target_fps: DEFAULT_TARGET_FPS as u32` (`:390`), and `preferred_video_codec: VideoCodec::AV1` (`:391`), waits `200 ms` for `SessionState::Active` (`:507-516`) plus `50 ms` for a frame (`:520-525`), and falls back to `"Connected"` with fabricated `60.0` FPS and `0.5 ms` RTT (`:528-553`) | UI-05 |
| L-UI-4: vendored `libcosmic` carries large local modifications (verify via item 3). | Confirmed | `vendor/libcosmic/Cargo.toml:1-30` and `vendor/libcosmic/src/{lib,layout,raster,runner}.rs`; `vendor/libcosmic` (`8,555` LOC) is not a modified upstream checkout at all, but a bespoke in-repo `taffy` + `tiny-skia` + `winit` + `wgpu` software renderer with 21 hardcoded `rferram-*` class constants (`layout.rs:18-78`) and 22 `rFerram` icon routines (`raster.rs:1386-1409`) | UI-04 |
| L-UI-5: UI fixtures use non-reserved private IPs (`app.rs:224 "192.168.1.100"`; `lib.rs:106, :119`). | Confirmed | `crates/rferram-ui/src/app.rs:210, 224` (default runtime cards use `"10.0.10.15:4433"` and `"192.168.1.100:4433"`), `crates/rferram-ui/src/lib.rs:106, 111, 119` (`"10.10.0.12:4433"`, `"/srv/devshare"`, `"100.64.1.25:4433"`), plus non-RFC-5737 IPs in `app.rs:977, 1122, 1159, 1217, 1640`, `modal.rs:370, 522`, `vendor/libcosmic/src/raster.rs:2520, 2604, 2696`, and `vendor/libcosmic/src/runner.rs:1717-1733` | UI-09 |

## Findings

### UI-01: Client session orchestration, VPN resolution, and channel demuxing live inside `rferram-ui` coupled to `cosmic` types
- Severity: High
- Category: structure
- Requirement: R2, R10, R11
- Evidence: `crates/rferram-ui/src/session_controller.rs:286-297, 330-483`
  ```rust
  pub fn spawn_connect_task(&self, profile: ConnectionProfile, vpn_binding: VpnAdapterBinding) -> Task<Message> {
      let controller = self.clone();
      Task::perform(async move { controller.connect_profile(profile, vpn_binding).await }, SessionConnectOutcome::into_message)
  ```
- Confidence: Read
- Impact: A CLI binary or replaceable frontend cannot establish a full session (VPN binding, DNS resolution, QUIC connect, bidirectional `VirtualChannelMessage` routing, and frame consumption) without duplicating `UiSessionController::connect_profile_inner` (`session_controller.rs:330-604`) or pulling in `rferram-ui` and `libcosmic`.
- Direction: Extract `VpnAdapterBinding`, endpoint address resolution, session connection orchestration, and bidirectional virtual-channel stream handling out of `rferram-ui` into `rferram-common` and `rferram-client`.

### UI-02: Profile persistence is locked to `cosmic-config` and never attached in the production `rferram-ui` binary
- Severity: High
- Category: gap
- Requirement: R2, R10
- Evidence: `crates/rferram-ui/src/app.rs:144-146, 244, 259-263`
  ```rust
  fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
      Self::new(core)
  }
  ```
- Confidence: Ran (`cargo test -p rferram-ui --locked`)
- Impact: Non-GUI frontends (`R2`) cannot read or write saved `ConnectionProfile` records without depending on `cosmic-config`, and because `RFerramApp::init` (`app.rs:144-146`) calls `Self::new(core)` (which sets `persistence_config: None` at `:244` and seeds two hardcoded dummy cards at `:205-234`) without calling `with_persistence_config(open_ui_config())`, running `rferram-ui` (`main.rs:65`) never loads or saves profiles on disk.
- Direction: Move profile persistence into a frontend-agnostic store in `rferram-common` and initialize the store during application startup rather than only in unit tests.

### UI-03: No active remote desktop viewport or peripheral input forwarding exists in `rferram-ui`
- Severity: High
- Category: gap
- Requirement: R6, R10
- Evidence: `crates/rferram-ui/src/app.rs:156-184, 872-879` and `crates/rferram-ui/src/session_controller.rs:198-205, 426-431`
  ```rust
  container("rferram-card-preview", row(0, vec![])),
  column(4, vec![text(&c.profile.friendly_name).size(14).into(), text("Saved Profile").size(11).into()]),
  ```
- Confidence: Read
- Impact: Connecting to a host never opens a remote desktop canvas or presents video frames—`latest_frame` and `latest_aligned_plane0` (`session_controller.rs:426-430`) are written in the background and never read outside tests (`:1080`), `ActiveUiSession::send_input` (`:198-205`) is never called outside tests (`:1115`), and connection cards don't even render `active_fps`, `active_rtt_ms`, or `session_statuses`.
- Direction: Implement an active session viewport surface in the viewer that presents decoded GPU frames and forwards window pointer/keyboard events to the client session pipeline, and render live status/telemetry on connection cards.

### UI-04: `vendor/libcosmic` is an in-repo 8,555-line software rasterizer masquerading as `libcosmic`
- Severity: High
- Category: structure
- Requirement: R10, R11, R12
- Evidence: `vendor/libcosmic/src/layout.rs:18-78` and `vendor/libcosmic/src/raster.rs:28-35, 2470-2473`
  ```rust
  pub const CLASS_ROOT_SHELL: &str = "rferram-root-shell";
  pub const CLASS_HEADER: &str = "rferram-header";
  pub const CLASS_TOP_TABBAR: &str = "rferram-top-tabbar";
  ```
- Confidence: Ran (`cargo test --manifest-path Cargo.toml -p libcosmic --locked`)
- Impact: `vendor/libcosmic` shares no code with System76's `pop-os/libcosmic`, embeds `rferram-*` layout rules (`layout.rs:18-78`) and `rFerram` icons (`raster.rs:1386-1409`) inside the toolkit crate, CPU-rasterizes the entire window on every redraw (`runner.rs:1258-1269`), renders any non-ASCII character as a box (`raster.rs:2470-2472`), and cycles dropdown options on click (`runner.rs:860`) instead of opening a menu.
- Direction: Decide at evaluation whether to replace `vendor/libcosmic` with real upstream `libcosmic`/`iced` (or `slint`) or rename and scope it honestly as an internal lightweight UI toolkit with real font and widget support.

### UI-05: `connect_profile_inner` hardcodes `AV1` and clipboard, blocks up to 250 ms, and fabricates fallback telemetry
- Severity: Medium
- Category: stub
- Requirement: R2, R3, R6
- Evidence: `crates/rferram-ui/src/session_controller.rs:388-392, 507-552`
  ```rust
  enable_clipboard: true,
  max_target_fps: DEFAULT_TARGET_FPS as u32,
  preferred_video_codec: VideoCodec::AV1,
  ```
- Confidence: Read
- Impact: User profile/modal settings cannot disable clipboard sync or change target FPS/codec, `connect_profile_inner` connects every peer with hardcoded TLS server name `"rferram.host"` (`:353`), and when the `200 ms` handshake wait (`:507-516`) expires without authentication, it still marks the session `"Connected"` (`:531`) and reports fake `active_fps = Some(60.0)` (`:542`) and `active_rtt_ms = Some(0.5)` (`:551`).
- Direction: Derive `SessionConfig` from profile settings, drive UI state transitions reactively from `state_rx` without fixed sleeps, and report `None` for FPS/RTT until real frames and measurements arrive.

### UI-06: Three separate Tokio runtimes and untracked background tasks leak across UI session lifecycles
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-ui/src/main.rs:38`, `crates/rferram-ui/src/session_controller.rs:224-238, 363, 411, 456, 489`, and `vendor/libcosmic/src/runner.rs:597-622`
  ```rust
  let rt = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(2)
      .enable_all()
  ```
- Confidence: Read
- Impact: `main.rs:38` starts a `#[tokio::main]` runtime, `SessionRuntimeHolder` (`session_controller.rs:229-234`) spawns a second multi-thread runtime, and `CosmicAppRunner::spawn_task` (`vendor/libcosmic/src/runner.rs:604-613`) spawns a new OS thread and a third `new_current_thread` Tokio runtime per `Task`; meanwhile, `connect_profile_inner` drops the `JoinHandle`s for its 3 spawned tasks (`:411, 456, 489`) and `start_metrics_monitor` (`:363`), and provides no UI action to disconnect an active session short of deleting the profile (`app.rs:565`).
- Direction: Share a single Tokio runtime handle across the client and UI, store and abort all session task handles on disconnect, and add an explicit `DisconnectSession` UI action.

### UI-07: Settings drawer, Help drawer, and multiple modal controls are non-functional UI stubs
- Severity: Medium
- Category: stub
- Requirement: R9, R10
- Evidence: `crates/rferram-ui/src/drawers.rs:107-124, 153-168` and `crates/rferram-ui/src/modal.rs:262-271`
  ```rust
  checkbox(
      "Stream Host Speaker Audio (PipeWire / WASAPI / CoreAudio)",
      self.add_modal_state.enable_speaker_audio,
      Message::SelectAddModalTab(AddDesktopModalTab::DevicesAudio),
  ),
  ```
- Confidence: Read
- Impact: Users cannot configure global settings or view help/diagnostics (`drawers.rs:107-124, 153-168` show only section title text and `"Close"`), clicking the audio/mic checkboxes in `Devices | Audio` (`modal.rs:262-271`) emits `SelectAddModalTab` instead of toggling audio settings, `Display` (`:247-254`) and `Credentials` (`:310`) dropdowns have no callbacks, `ConfigureConnection` (`app.rs:497-524`) followed by `"Add"` (`:431-466`) duplicates the profile instead of editing it, and `Import`/`Export` (`:356-394, 543-560`) never touch the filesystem.
- Direction: Wire modal controls to real state mutations and `CredentialVault`, update existing profiles in place when saving from `ConfigureConnection`, implement file-backed import/export, and populate or remove placeholder drawer panes.

### UI-08: `FROSTED_GLASS_SHADER_WGSL` is unused in rendering and implements only a flat alpha tint
- Severity: Low
- Category: stub
- Requirement: R10
- Evidence: `crates/rferram-ui/src/shader.rs:22-26` and `crates/rferram-ui/src/main.rs:13-36`
  ```rust
  @fragment
  fn fs_frosted_glass(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
      let alpha = clamp(glass.opacity, 0.0, 1.0);
      return vec4<f32>(glass.tint_color.rgb, alpha);
  }
  ```
- Confidence: Read
- Impact: `FROSTED_GLASS_SHADER_WGSL` is only string-checked at startup (`main.rs:14-28`) and never compiled into a render pipeline outside unit tests (`shader.rs:149-176`), while its fragment shader ignores `blur_radius`, `corner_radius`, and `frag_coord`.
- Direction: Remove the dead shader module if the UI delegates styling to the widget toolkit, or wire it into an actual GPU compositor pass.

### UI-09: Production default cards and UI test fixtures contain non-reserved private IPs and `/srv/devshare`
- Severity: Low
- Category: sensitive-data
- Requirement: R10
- Evidence: `crates/rferram-ui/src/app.rs:210, 224` and `crates/rferram-ui/src/lib.rs:106, 111, 119`
  ```rust
  host_address: String::from("192.168.1.100:4433"),
  ```
- Confidence: Read
- Impact: Default runtime cards in `RFerramApp::new` (`app.rs:210, 224`) and unit tests across `rferram-ui` (`lib.rs:106, 111, 119, 146, 226`, `app.rs:977, 1122, 1159, 1217, 1640`, `modal.rs:370, 522`) and `vendor/libcosmic` (`raster.rs:2520, 2604, 2696`, `runner.rs:1717-1733`) use RFC 1918 / RFC 6598 addresses (`10.x.x.x`, `192.168.1.100`, `100.64.1.25`) and internal path `/srv/devshare` instead of reserved RFC 5737 documentation addresses (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`).
- Direction: Replace all non-loopback IP literals and `/srv/devshare` paths in UI defaults and tests with RFC 5737 documentation addresses and neutral placeholder paths.

## Open questions for Claude
1. Should `vendor/libcosmic` (`8,555` LOC custom `taffy` + `tiny-skia` + `winit` + `wgpu` software renderer) be discarded in favor of upstream System76 `libcosmic` (or `slint`), or kept and renamed as an internal lightweight UI shell once real font rendering (`cosmic-text`) and a GPU video viewport are added?
2. Should `ConnectionProfile` persistence live in `rferram-common` as a plain JSON/TOML file under XDG config directories so both headless CLI (`R2`) and GUI (`R10`) share the same profile store without `cosmic-config`?
