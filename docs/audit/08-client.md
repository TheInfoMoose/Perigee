# 08 Client Pipeline, Render, and E2E Loopback audit
Audited HEAD: ee63421

## Summary
- `rferram-client` is a library crate with no CLI binary (`crates/rferram-client/Cargo.toml:1-20`); `QuicSessionBridge::spawn` (`crates/rferram-client/src/bridge.rs:64-72`) is the nearest programmatic entry point, but `ClientSessionPipeline` only emits an unauthenticated `ClientHello` (`crates/rferram-client/src/session.rs:150-164`) and ignores `ServerHello`, `AuthResponse { success: false, .. }`, `ControlMessage::Disconnect`, `WireMessage::Topology`, and `WireMessage::Channel` (`session.rs:186-194, 273-286`).
- No video decoder exists in `rferram-client` or the workspace: `ClientSessionPipeline::demux_loop` moves raw `VideoFrameMessage.payload` bytes straight into `DecodedFrameSurface.data` without inspecting `frame.codec` (`session.rs:217-225`), and reports `mpsc` channel send duration as both `render_latency_us` and `FrameAck.render_delay_us` (`session.rs:226-252`).
- `crates/rferram-client/src/render.rs:1-782` contains only WGSL source validation, CPU-side plane stride/layout math, and `wgpu` descriptor builders (`WgpuRenderPipeline` holds no `wgpu::Instance`, `Adapter`, `Device`, `Queue`, `Surface`, or `RenderPipeline`); `wgpu` device creation and `queue.write_texture` exist only inside a unit test (`render.rs:1114-1161`), and no render pass or draw call exists anywhere in the crate.
- `crates/rferram-client/src/audio.rs:1-184` is an in-memory `VecDeque<AudioFrameMessage>` ring buffer (`ClientAudioBuffer`) with no Opus/PCM decoder or OS audio device (`cpal`, `PipeWire`, `WASAPI`, `CoreAudio`); incoming audio frames pushed by `demux_loop` (`session.rs:266-271`) are never popped outside tests.
- `crates/rferram-client/tests/e2e_loopback.rs:38-208` tests against an in-test mock `HostSessionEngine` rather than `rferram-server`, and manually injects `HandshakeMessage::AuthRequest` on `bridge.pipeline.wire_tx` (`e2e_loopback.rs:419-430`) because `ClientSessionPipeline` cannot perform challenge-response authentication on its own.

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| Connect API usable without a GUI | Partial | `crates/rferram-client/src/bridge.rs:64-146` (`QuicSessionBridge::spawn`) and `crates/rferram-client/src/session.rs:85-168` (`ClientSessionPipeline::spawn`) do not depend on `libcosmic`, but no headless CLI binary exists (`crates/rferram-client/Cargo.toml:1-20`), and callers must manually wire `rferram_net::QuicClientTransport`, authentication, and channel handling (`crates/rferram-ui/src/session_controller.rs:330-599`) | R2, R11 |
| Handshake / auth | Partial | `crates/rferram-client/src/session.rs:145-165, 186-191`; `spawn` sends `HandshakeMessage::ClientHello`, and `demux_loop` sets `SessionState::Active` on `AuthResponse { success: true, .. }`, but ignores `ServerHello` challenges and `AuthResponse { success: false, .. }`, and never sends `AuthRequest` | R2, R3 |
| Frame receive | Real | `crates/rferram-client/src/bridge.rs:117-137` (`recv_uni_message` loop) and `crates/rferram-client/src/session.rs:195-254` (`WireMessage::Video(frame)` branch in `demux_loop`) receive length-prefixed `VideoFrameMessage` frames and forward `DecodedFrameSurface` on `video_rx` | R2, R6 |
| Loss detection + `LossReport` | Partial | `crates/rferram-client/src/session.rs:181, 200-215`; detects `frame_idx > expected` and sends `ControlMessage::LossReport` over a QUIC uni stream (`bridge.rs:36-59`), but misses initial frame loss (`expected_frame_id` starts at `None`), allocates `(expected..frame_idx).collect()` without bounds, and resets `expected_frame_id` backward on out-of-order frames | R2, R6 |
| `FrameAck` | Partial | `crates/rferram-client/src/session.rs:247-252` and `crates/rferram-client/src/bridge.rs:36-54`; emits `ControlMessage::FrameAck` over a QUIC UDP datagram after `video_tx.send`, but `render_delay_us` only measures `mpsc` enqueue time (`decode_start.elapsed()`) before any decode or GPU presentation | R2, R6 |
| Decode | Missing | `crates/rferram-client/src/session.rs:217-226`; `frame.payload` is moved unaltered into `DecodedFrameSurface.data` regardless of `frame.codec` (`AV1`, `H265`, `RawBgra`), and no video decoder crate exists in `Cargo.toml` | R2, R6 |
| Render (`wgpu` device/surface/draw) | Stub | `crates/rferram-client/src/render.rs:218-782`; `WgpuRenderPipeline` only validates `VIDEO_SURFACE_SHADER_WGSL` and computes CPU-side plane strides/layouts (`TextureSurfaceLayout`); `wgpu::Instance`/`Device`/`Queue`/`Texture` are used only in `#[cfg(test)]` (`render.rs:1114-1161`), and no `wgpu::Surface`, `RenderPipeline`, or draw call exists anywhere | R2, R6, R12 |
| Input send | Partial | `crates/rferram-client/src/session.rs:131-141, 311-359` and `crates/rferram-client/src/bridge.rs:36-54`; `send_mouse_move`, `send_mouse_button`, and `send_key_event` forward `WireMessage::Input` over QUIC datagrams, but silently ignore closed-channel errors and omit helpers for `MouseScroll` and `UnicodeText` | R2, R6 |
| Audio playback | Stub | `crates/rferram-client/src/audio.rs:44-184` and `crates/rferram-client/src/session.rs:255-272, 292-308`; `ClientAudioBuffer` is an in-memory `VecDeque<AudioFrameMessage>` (capacity 32) with no Opus/PCM decoder or OS audio output/input stream, and nothing in non-test code ever pops from `pipeline.audio_buffer` | R6, R12 |
| Disconnect | Partial | `crates/rferram-client/src/bridge.rs:158-161`; `QuicSessionBridge::stop()` stores `false` to `is_running`, but does not send `ControlMessage::Disconnect`, does not close the underlying `quinn::Connection`, does not abort/wake the 6 spawned tasks waiting on `.recv().await`, and `demux_loop` ignores incoming `ControlMessage::Disconnect` (`session.rs:281`) | R7 |
| Reconnect | Missing | `crates/rferram-client/src/session.rs:69`; `SessionState::Reconnecting { attempt, reason }` is defined in the enum but never constructed or handled anywhere in the workspace | R7 |

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-CLI-1: there is no headless client binary; `QuicSessionBridge::spawn` (`bridge.rs:64`) is the nearest entry point. | Confirmed | `crates/rferram-client/Cargo.toml:1-20` defines only a library target (`src/lib.rs`); `QuicSessionBridge::spawn(config: SessionConfig, connection: Arc<QuicConnectionManager>)` (`crates/rferram-client/src/bridge.rs:64-72`) is the highest-level entry point in `rferram-client`, while VPN interface resolution, vault signing, and channel wiring remain inside `rferram-ui` (`crates/rferram-ui/src/session_controller.rs:330-599`) | CLI-01 |
| L-CLI-2: no decoder exists; the payload goes straight into `DecodedFrameSurface` (`session.rs:217-224`). | Confirmed | `crates/rferram-client/src/session.rs:217-226` moves `data: frame.payload` directly into `DecodedFrameSurface` without checking `frame.codec` or `frame.is_keyframe`, and records the struct construction time as `decode_latency_us` (`:226, 236`) | CLI-02 |
| L-CLI-3: `render.rs` has no `wgpu` device/queue/surface/draw outside tests (`:614`, `:1115+`). | Confirmed | `crates/rferram-client/src/render.rs:218-224` (`WgpuRenderPipeline` stores only `width`, `height`, `pixel_format`, `layout`, `vsync_enabled`); non-test code up to `:782` only builds `wgpu::TextureDescriptor` (`:614-628`) and `wgpu::ShaderModuleDescriptor` (`:683-781`), while `wgpu::Instance::default()`, `request_adapter`, `request_device`, `create_shader_module`, `create_texture`, and `queue.write_texture` appear only inside `#[cfg(test)]` at `render.rs:1114-1161` (with no `Surface`, `RenderPipeline`, or draw call even in tests) | CLI-03, CLI-04 |
| L-CLI-4: `e2e_loopback.rs` uses an in-test mock host, not `rferram-server`. | Confirmed | `crates/rferram-client/tests/e2e_loopback.rs:38-208` defines a standalone `HostSessionEngine` inside the test file rather than importing `rferram-server`, and lines `398-430` manually sign a hardcoded challenge (`b"rferram-e2e-loopback-challenge-32b"`) and inject `HandshakeMessage::AuthRequest` into `bridge.pipeline.wire_tx` without reading `ServerHello` | CLI-05, CLI-06 |
| L-CLI-5: client default target is `127.0.0.1:4433` (`session.rs:39`); client `audio.rs` status. | Confirmed | `crates/rferram-client/src/session.rs:39` sets `host_address: String::from("127.0.0.1:4433")` in `SessionConfig::default()` (used only in an `info!` log at `:147`), and `crates/rferram-client/src/audio.rs:44-184` implements only a 32-frame `VecDeque<AudioFrameMessage>` (`ClientAudioBuffer`) that `demux_loop` pushes into (`session.rs:266-271`) and nothing in production code ever drains | CLI-07 |

## Headless client entry point

### Smallest existing public API surface for a CLI client (`rferram-net` + `rferram-client`)

A headless CLI binary (`R2`) built today against `rferram-net`, `rferram-client`, and `rferram-protocol` would call the following existing public items:

1. **Configure and connect QUIC transport (`rferram-net`)**:
   - `rferram_net::QuicTransportConfig::default() -> QuicTransportConfig` (`crates/rferram-net/src/lib.rs:25-35`)
   - `rferram_net::QuicClientTransport::new(config: QuicTransportConfig) -> QuicClientTransport` (`crates/rferram-net/src/endpoint.rs:69-71`)
   - `QuicClientTransport::connect(&self, server_addr: SocketAddr, server_name: &str) -> Result<QuicConnectionManager>` (`crates/rferram-net/src/endpoint.rs:76-81`) or `QuicClientTransport::connect_on_interface(&self, server_addr: SocketAddr, server_name: &str, interface_or_ip: Option<&str>) -> Result<QuicConnectionManager>` (`crates/rferram-net/src/endpoint.rs:86-114`)
2. **Spawn the session bridge and pipeline (`rferram-client`)**:
   - `rferram_client::SessionConfig` (`crates/rferram-client/src/session.rs:24-33`)
   - `rferram_client::QuicSessionBridge::spawn(config: SessionConfig, connection: Arc<QuicConnectionManager>) -> (QuicSessionBridge, watch::Receiver<SessionState>, mpsc::Receiver<DecodedFrameSurface>, mpsc::Sender<InputEvent>)` (`crates/rferram-client/src/bridge.rs:64-72`)
   - `QuicSessionBridge::peer_cert_sha256(&self) -> Option<[u8; 32]>` (`crates/rferram-client/src/bridge.rs:149-151`)
3. **Authenticate (`rferram-common` + `rferram-client`)**:
   - `QuicSessionBridge::spawn` automatically sends `WireMessage::Handshake(HandshakeMessage::ClientHello { .. })` (`crates/rferram-client/src/session.rs:150-164`).
   - If sending `AuthRequest` manually: `rferram_common::NistVault::sign_channel_bound_challenge` (`crates/rferram-common/src/vault.rs:116-123`) sent via `bridge.pipeline.wire_tx.send(WireMessage::Handshake(HandshakeMessage::AuthRequest { .. })).await` (`crates/rferram-client/src/session.rs:79`).
   - Observe `watch::Receiver<SessionState>::changed().await` until `*state_rx.borrow() == SessionState::Active` (`crates/rferram-client/src/session.rs:64-71, 188-190`).
4. **Receive frames and send input (`rferram-client`)**:
   - Receive frames: `video_rx.recv().await -> Option<DecodedFrameSurface>` (`crates/rferram-client/src/render.rs:48-55`) and validate layout via `DecodedFrameSurface::validate(&self) -> Result<TextureSurfaceLayout>` (`crates/rferram-client/src/render.rs:64-66`).
   - Send input: `ClientSessionPipeline::send_mouse_move(&self, monitor_id: u32, x: f64, y: f64, is_absolute: bool)` (`crates/rferram-client/src/session.rs:311`), `send_mouse_button(&self, button: MouseButtonCode, state: ElementState, x: f64, y: f64)` (`:324-330`), `send_key_event(&self, key_code: u32, scancode: u32, state: ElementState, modifiers: KeyModifiers)` (`:343-349`), or `input_tx.send(InputEvent).await` (`:94`).
5. **Disconnect (`rferram-client`)**:
   - `QuicSessionBridge::stop(&self)` (`crates/rferram-client/src/bridge.rs:159-161`).

### What is missing for a functional CLI client (R2)
- **No CLI binary target**: Neither `rferram-client` nor a dedicated CLI crate provides a `main.rs` or argument parser.
- **Connect orchestration lives in `rferram-ui`**: VPN interface resolution (`VpnInterfaceDiscovery`), vault/token credential loading, channel manager startup (`VirtualChannelManager`), and the handshake timeout live in `crates/rferram-ui/src/session_controller.rs:330-599` instead of `rferram-client`.
- **Handshake state machine is incomplete**: `ClientSessionPipeline::demux_loop` ignores `HandshakeMessage::ServerHello` (never signing `auth_challenge` to send `AuthRequest`) and ignores `AuthResponse { success: false, .. }`.
- **No video decoding or presentation/output sink**: `DecodedFrameSurface.data` contains undecoded wire payload bytes, and `WgpuRenderPipeline` has no `wgpu` device/surface/render pass (nor a headless frame dump/windowing sink).
- **Virtual channels and topology are dropped**: `demux_loop` logs and discards `WireMessage::Channel` (`session.rs:283-285`) and `WireMessage::Topology` (`:192-194`).
- **No clean disconnect**: `QuicSessionBridge::stop()` neither sends `ControlMessage::Disconnect` nor closes the `quinn::Connection` or aborts the spawned bridge/pipeline tasks.

### Test execution verification (`cargo test -p rferram-client --locked`)
- **Command run**: `cargo test -p rferram-client --locked` (exit `0`).
- **Results**: 14 unit tests (`src/lib.rs`) + 1 integration test (`tests/e2e_loopback.rs`) = 15 passed, 0 failed, 0 ignored.
- **What the tests actually prove**:
  - `audio::tests` (4 tests): `VecDeque` enqueue/dequeue, capacity-3 overflow eviction, direction/empty validation, and `build_microphone_frame` struct construction.
  - `bridge::tests` (2 tests): `is_datagram_message` classification (`Input` and `FrameAck` -> datagram; others -> uni stream) and a local `127.0.0.1:0` QUIC loopback verifying `ClientHello`, `AuthResponse`, `VideoFrameMessage` (#1 and #3 -> `LossReport { missing_frames: [2] }` + `FrameAck`), `AudioFrameMessage` enqueue, and `InputEvent` datagram delivery.
  - `render::tests` (5 tests): WGSL comment stripping and substring token checks, plane stride/offset arithmetic for `Bgra8`/`Rgba8`/`NV12`/`YUV420P`/`YUV444P`, 256-byte row alignment repacking, and (when a GPU adapter is present, here Mesa `radv`) `device.create_shader_module` + `queue.write_texture` of a constant `0x80` buffer without creating a render pipeline or drawing.
  - `session::tests` (2 tests): in-memory `mpsc` channel tests for `ClientHello`, `LossReport` on frame 1 -> 3 gap, `FrameAck`, `AuthResponse` -> `SessionState::Active`, `BandwidthEstimate` -> `rtt_ms`, and input forwarding.
  - `tests/e2e_loopback.rs` (1 test): QUIC loopback against a test-local mock `HostSessionEngine` where the test body manually signs a static challenge and injects `AuthRequest` into `wire_tx`.

## Findings

### CLI-01: No headless client binary exists, and session setup/channel orchestration is trapped in `rferram-ui` instead of `rferram-client`
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
- Impact: `rferram-client` has no binary target (`crates/rferram-client/Cargo.toml:1-20`), declares `rferram-channels` (`Cargo.toml:14`) without importing it anywhere in `src/`, discards `WireMessage::Channel` and `WireMessage::Topology` in `demux_loop` (`session.rs:192-194, 283-285`), and leaves VPN adapter binding, credential signing, and virtual channel routing inside `crates/rferram-ui/src/session_controller.rs:330-599`.
- Direction: Extract connection setup, authentication, and virtual channel routing from `rferram-ui` into `rferram-client` and add a headless CLI binary target that can connect, stream, and interact without a GUI.

### CLI-02: `ClientSessionPipeline` has no video decoder, passes raw wire payloads as `DecodedFrameSurface`, and reports `mpsc` enqueue time as `render_delay_us`
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-client/src/session.rs:217-226`
```rust
                    let decode_start = Instant::now();
                    let decoded_surface = DecodedFrameSurface {
                        frame_index: frame_idx,
```
- Confidence: Read
- Impact: `demux_loop` (`session.rs:217-252`) ignores `frame.codec`, `frame.is_keyframe`, and `frame.stride`, moves `frame.payload` directly into `DecodedFrameSurface.data` without calling `DecodedFrameSurface::validate()`, and sets `decode_latency_us`, `render_latency_us`, and `ControlMessage::FrameAck.render_delay_us` to the elapsed microseconds of constructing the struct and pushing it into `video_tx`.
- Direction: Integrate a real video decoder (`AV1` / `H265` / `RawBgra` validation) in the client media pipeline and emit `FrameAck` with actual decode and GPU presentation timestamps after frame upload/draw.

### CLI-03: `WgpuRenderPipeline` contains no `wgpu` device, queue, surface, render pipeline, or draw call outside unit tests, and declares an unused `bytemuck` dependency
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-client/src/render.rs:218-224`
```rust
pub struct WgpuRenderPipeline {
    pub width: u32,
    pub height: u32,
```
- Confidence: Read
- Impact: Outside `#[cfg(test)]` (`render.rs:1114-1161`), `render.rs` never creates a `wgpu::Instance`, `Adapter`, `Device`, `Queue`, `Surface`, `BindGroup`, or `RenderPipeline` and never issues a draw call (settling the `01-baseline.md:109` contradiction); additionally, `bytemuck` is declared in `crates/rferram-client/Cargo.toml:13` but never used in `crates/rferram-client/src/`.
- Direction: Implement real `wgpu` device/queue/surface management, bind group creation, and render pass submission in `WgpuRenderPipeline` (or remove unused `bytemuck` if no vertex/uniform buffers require it).

### CLI-04: `VIDEO_SURFACE_SHADER_WGSL` only binds a single 2D texture and cannot sample multi-planar `NV12`, `YUV420P`, or `YUV444P` frames
- Severity: Medium
- Category: gap
- Requirement: R6
- Evidence: `crates/rferram-client/src/render.rs:29-37`
```rust
@group(0) @binding(0)
var frame_tex: texture_2d<f32>;
@group(0) @binding(1)
```
- Confidence: Read
- Impact: While `WgpuRenderPipeline::calculate_surface_layout` (`render.rs:357-420`) produces 2 planes (`R8Unorm` + `Rg8Unorm`, with `primary_wgpu_format = TextureFormat::NV12`) for `NV12` and 3 `R8Unorm` planes for `YUV420P`/`YUV444P`, `VIDEO_SURFACE_SHADER_WGSL` (`render.rs:13-38`) defines only one `@group(0) @binding(0) var frame_tex: texture_2d<f32>` with no chroma bindings or YUV-to-RGB color-space conversion matrix.
- Direction: Add multi-planar Y/UV/U/V texture bindings and BT.709/BT.601 YUV-to-RGB conversion in `VIDEO_SURFACE_SHADER_WGSL` matching the plane layouts generated by `calculate_surface_layout`.

### CLI-05: `ClientSessionPipeline` never handles `ServerHello` or `AuthResponse { success: false }`, and leaves 4 of 6 `SessionState` variants dead
- Severity: High
- Category: gap
- Requirement: R3
- Evidence: `crates/rferram-client/src/session.rs:186-191`
```rust
                WireMessage::Handshake(h) => {
                    info!(?h, "Handshake update received");
                    if matches!(h, HandshakeMessage::AuthResponse { success: true, .. }) {
```
- Confidence: Read
- Impact: `ClientSessionPipeline::spawn` sends only a bare `ClientHello` (`session.rs:150-164`); when a host replies with `ServerHello { auth_challenge, .. }` or `AuthResponse { success: false, .. }`, `demux_loop` takes no action, never sends `AuthRequest`, and never transitions to `SessionState::Authenticating`, `NegotiatingTopology`, `Reconnecting`, or `Terminated` (`session.rs:65-70`).
- Direction: Drive the full mutual-authentication state machine inside `ClientSessionPipeline` (`Connecting` -> `Authenticating` on `ServerHello` -> `AuthRequest` -> `Active` or `Terminated` on `AuthResponse`).

### CLI-06: `tests/e2e_loopback.rs` tests against an in-test mock `HostSessionEngine` instead of `rferram-server` and manually bypasses client handshake handling
- Severity: Medium
- Category: gap
- Requirement: R2
- Evidence: `crates/rferram-client/tests/e2e_loopback.rs:38-41`
```rust
pub struct HostSessionEngine {
    pub session_id: String,
    pub server_cert_sha256: [u8; 32],
```
- Confidence: Ran (`cargo test -p rferram-client --locked`)
- Impact: `e2e_loopback.rs` defines its own mock `HostSessionEngine` (`:38-208`) whose handshake protocol (`ClientHello` -> `ServerHello` -> `AuthRequest` -> `AuthResponse`) differs from `rferram-server`'s `daemon.rs`, and the test body (`:398-430`) manually signs a hardcoded challenge string and pushes `AuthRequest` onto `bridge.pipeline.wire_tx` without reading `ServerHello`, masking the fact that `ClientSessionPipeline` cannot complete `ServerHello` authentication.
- Direction: Replace the mock `HostSessionEngine` in `e2e_loopback.rs` with an integration test against the real host session engine once client and server share a single authenticated handshake flow.

### CLI-07: `ClientAudioBuffer` has no audio decoder or OS audio device integration, and buffered speaker frames are never popped outside tests
- Severity: Medium
- Category: stub
- Requirement: R6
- Evidence: `crates/rferram-client/src/session.rs:266-271`
```rust
                    if enable_audio {
                        let mut buf = audio_buffer.lock().await;
                        if let Err(e) = buf.push_frame(audio) {
```
- Confidence: Read
- Impact: Despite `crates/rferram-client/src/lib.rs:4` claiming `audio output playback (cpal / CoreAudio / WASAPI / PipeWire)`, `audio.rs` only pushes raw `AudioFrameMessage` structs into a 32-slot `VecDeque` (`ClientAudioBuffer`), and no non-test code ever calls `pop_frame()`, causing every session with audio enabled to fill the 32-frame buffer and silently evict every subsequent frame.
- Direction: Wire an Opus decoder and cross-platform audio output/input stream to drain `ClientAudioBuffer`, or disable/defer audio until video and input are complete.

### CLI-08: Sequence gap detection in `demux_loop` allows unbounded `Vec<u64>` allocation on peer-controlled `frame_id` and mishandles initial or out-of-order frames
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-client/src/session.rs:201-205`
```rust
                    if let Some(expected) = expected_frame_id
                        && frame_idx > expected
                    {
                        let missing: Vec<u64> = (expected..frame_idx).collect();
```
- Confidence: Read
- Impact: Because `frame_idx` comes directly from `VideoFrameMessage.frame_id`, a peer sending `frame_id = 1` followed by a large `frame_id` (e.g., `u64::MAX`) causes `(expected..frame_idx).collect()` to attempt a multi-gigabyte allocation and abort the client (and `frame_idx + 1` at `:215` overflows on `u64::MAX`); additionally, `expected_frame_id` starts at `None` (`:181`, missing loss of frame 1) and rewinds backward if frames arrive out of order.
- Direction: Initialize `expected_frame_id` to `Some(1)`, use `saturating_add(1)` with monotonic high-water tracking, and cap `missing_frames` range expansion (e.g., `<= MAX_LOSS_REPORT_FRAMES`) before allocating.

### CLI-09: `QuicSessionBridge::stop()` and `ClientSessionPipeline` leak detached tasks on disconnect and never close the QUIC connection
- Severity: Medium
- Category: lifecycle
- Requirement: R7
- Evidence: `crates/rferram-client/src/bridge.rs:158-161`
```rust
    /// Signals all bridge pump tasks to stop.
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
```
- Confidence: Read
- Impact: `ClientSessionPipeline::spawn` (`session.rs:118-165`) and `QuicSessionBridge::spawn` (`bridge.rs:84-137`) spawn 6 detached `tokio::spawn` tasks without storing `JoinHandle`s; calling `bridge.stop()` only sets `is_running = false` without sending `ControlMessage::Disconnect`, closing the `quinn::Connection`, or waking tasks blocked on `outbound_rx.recv().await`, `recv_datagram().await`, or `recv_uni_message().await`, and `demux_loop` ignores incoming `ControlMessage::Disconnect` (`session.rs:281`).
- Direction: Send `ControlMessage::Disconnect` and call `connection.close(...)` on teardown, abort or select on a cancellation token across all bridge/pipeline tasks, and transition `state_tx` to `SessionState::Terminated` when the transport or peer disconnects.

### CLI-10: `ClientSessionPipeline::demux_loop` logs full `HandshakeMessage` and `VirtualChannelMessage` payloads at `INFO` level
- Severity: Medium
- Category: security
- Requirement: R3
- Evidence: `crates/rferram-client/src/session.rs:186-188`
```rust
                WireMessage::Handshake(h) => {
                    info!(?h, "Handshake update received");
```
- Confidence: Read
- Impact: `info!(?h, "Handshake update received")` (`session.rs:187`) and `info!(?vc, "VirtualChannelMessage received")` (`session.rs:284`) format the entire enum with `Debug` at `INFO` level, writing `AuthResponse.session_token`, `AuthRequest.credentials` (if received), user clipboard text/image bytes, and file transfer data into standard client logs.
- Direction: Redact `HandshakeMessage` tokens/credentials and `VirtualChannelMessage` payloads in `Debug` implementations or log only message variant names and byte lengths.

## Open questions for Claude
1. Should the headless CLI client (`R2`) live as `crates/rferram-client/src/main.rs` (`[[bin]] name = "rferram-client"`) or as a separate binary crate, and what output mode should it support when run without a display server (e.g., headless frame verification/recording vs. optional `wgpu`/Wayland/X11 window)?
2. For client video decoding (`CLI-02`), which software AV1 decoder crate (e.g., `rav1d` or `dav1d`) should be added to the workspace alongside `rav1e`, or will the initial milestone use a different codec/pipeline?
3. When `connect_profile_inner` is extracted from `rferram-ui` (`STEP 1I`), should `QuicSessionBridge::spawn` be replaced by a higher-level `ClientSession::connect(config)` API in `rferram-client` that owns transport creation, mutual authentication, virtual channel routing, and clean teardown?
