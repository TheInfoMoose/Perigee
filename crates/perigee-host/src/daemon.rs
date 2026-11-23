//! Elevated Host Session Controller & Daemon Service Orchestrator.
//!
//! Coordinates per-session GPU frame capture, hardware/software video encoding,
//! OS input injection, and 20 ms Opus audio loopback. Peers are authenticated by mutual TLS
//! in `perigee-net` before a session handshake starts; the daemon only validates the
//! `ClientHello`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{debug, error, info, warn};

use perigee_channels::VirtualChannelManager;
use perigee_common::RuntimeEnvironment;
use perigee_net::{QuicConnectionManager, node_key_to_hex};
use perigee_protocol::{
    ControlMessage, HandshakeMessage, VideoCodec, WireMessage, decode_length_prefixed,
};

use crate::audio::HostAudioCapturer;
use crate::backends::{
    FrameCapturer, LinuxCaptureController, MacOsScreenCaptureKitCapturer, WindowsDxgiCapturer,
};
use crate::encoder::{EncoderConfig, MacOsVideoToolboxEncoder, SoftwareVideoEncoder, VideoEncoder};
use crate::{CaptureBackendType, HostInputInjector, MediaMode, NativeInputInjector};

/// Frames per second of live (raw-frame) media sessions.
pub const LIVE_RAW_FPS: u32 = 10;

/// Maximum number of encoded video frames buffered for outbound QUIC transmission before dropping.
pub(crate) const MAX_OUTBOUND_VIDEO_BACKLOG: usize = 2;

/// Called with the size of each frame sent to the client, so pixel-space input can be scaled.
type ScreenSink = Arc<dyn Fn(u32, u32) + Send + Sync>;

/// Called when a session stops, to end a capture source the capture thread may be blocked on.
type CaptureStop = Arc<dyn Fn() + Send + Sync>;

/// A session's capturer, encoder, input injector, optional screen-size sink and optional capture stop.
type SessionMedia = (
    Box<dyn FrameCapturer>,
    Box<dyn VideoEncoder>,
    Arc<dyn HostInputInjector>,
    Option<ScreenSink>,
    Option<CaptureStop>,
);

/// Active Session Controller on Host Daemon
pub struct HostSessionEngine {
    pub session_id: String,
    pub backend_type: CaptureBackendType,
    pub capturer: Arc<Mutex<Box<dyn FrameCapturer>>>,
    pub encoder: Arc<Mutex<Box<dyn VideoEncoder>>>,
    pub input_injector: Arc<dyn HostInputInjector>,
    pub audio_capturer: HostAudioCapturer,
    pub channel_manager: Arc<VirtualChannelManager>,
    pub force_keyframe_flag: Arc<AtomicBool>,
    pub last_acked_frame_id: Arc<AtomicU64>,
    pub is_active: Arc<AtomicBool>,
    stop_tx: Arc<watch::Sender<bool>>,
    frame_interval: Duration,
    screen_sink: Option<ScreenSink>,
    capture_stop: Option<CaptureStop>,
}

impl HostSessionEngine {
    /// Instantiates the platform [`FrameCapturer`] implementation matching `backend_type`.
    pub fn build_frame_capturer(
        backend_type: CaptureBackendType,
        display_id: u32,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn FrameCapturer>> {
        let w = width.max(1);
        let h = height.max(1);
        match backend_type {
            CaptureBackendType::WaylandPipeWire | CaptureBackendType::X11Fallback => Ok(Box::new(
                LinuxCaptureController::new(backend_type, display_id, w, h)?,
            )),
            CaptureBackendType::MacOsScreenCaptureKit => Ok(Box::new(
                MacOsScreenCaptureKitCapturer::new(display_id, w, h)?,
            )),
            CaptureBackendType::WindowsDxgi => {
                Ok(Box::new(WindowsDxgiCapturer::new(display_id, w, h)?))
            }
        }
    }

    /// Creates a new [`HostSessionEngine`] for an authenticated remote viewer session.
    pub fn new(
        session_id: String,
        backend_type: CaptureBackendType,
        encoder_config: EncoderConfig,
    ) -> Result<Self> {
        Self::with_media(session_id, backend_type, encoder_config, MediaMode::StandIn)
    }

    /// Creates a [`HostSessionEngine`] whose capture, encoder and input follow `media`.
    pub fn with_media(
        session_id: String,
        backend_type: CaptureBackendType,
        encoder_config: EncoderConfig,
        media: MediaMode,
    ) -> Result<Self> {
        let fps = match media {
            MediaMode::StandIn => encoder_config.target_fps,
            MediaMode::Live | MediaMode::FollowSeat => LIVE_RAW_FPS,
        };
        let frame_interval = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
        let (capturer, encoder, input_injector, screen_sink, capture_stop) = match media {
            MediaMode::StandIn => {
                let width = encoder_config.width.max(1);
                let height = encoder_config.height.max(1);
                let capturer = Self::build_frame_capturer(backend_type, 1, width, height)
                    .context("failed to start the frame capturer for this session")?;
                let encoder: Box<dyn VideoEncoder> = match backend_type {
                    CaptureBackendType::MacOsScreenCaptureKit => {
                        Box::new(MacOsVideoToolboxEncoder::new(encoder_config))
                    }
                    _ => Box::new(SoftwareVideoEncoder::new(encoder_config)),
                };
                let injector: Arc<dyn HostInputInjector> =
                    Arc::new(NativeInputInjector::new(backend_type));
                (capturer, encoder, injector, None, None)
            }
            MediaMode::Live => Self::build_live_media(encoder_config)?,
            MediaMode::FollowSeat => Self::build_follow_seat_media(encoder_config)?,
        };
        let (stop_tx, _) = watch::channel(false);

        Ok(Self {
            session_id,
            backend_type,
            capturer: Arc::new(Mutex::new(capturer)),
            encoder: Arc::new(Mutex::new(encoder)),
            input_injector,
            audio_capturer: HostAudioCapturer::new(backend_type),
            channel_manager: Arc::new(VirtualChannelManager::new()),
            force_keyframe_flag: Arc::new(AtomicBool::new(true)),
            last_acked_frame_id: Arc::new(AtomicU64::new(0)),
            is_active: Arc::new(AtomicBool::new(true)),
            stop_tx: Arc::new(stop_tx),
            frame_interval,
            screen_sink,
            capture_stop,
        })
    }

    /// Linux live media: Wayland shared-memory capture of output 0, raw frames, uinput input.
    #[cfg(target_os = "linux")]
    fn build_live_media(encoder_config: EncoderConfig) -> Result<SessionMedia> {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .context("live media needs XDG_RUNTIME_DIR for its shared-memory buffer")?;
        let shm_capturer = crate::backends::WaylandShmCapturer::connect(None, &runtime_dir, 0)
            .context("failed to start Wayland capture (is WAYLAND_DISPLAY set?)")?;
        let cancel = shm_capturer.canceller();
        let capture_stop: CaptureStop = Arc::new(move || cancel.cancel());
        let uinput = Arc::new(
            crate::backends::UinputInjector::new(shm_capturer.width(), shm_capturer.height())
                .context("failed to create the /dev/uinput devices")?,
        );
        let sink_target = uinput.clone();
        let screen_sink: ScreenSink = Arc::new(move |width, height| {
            sink_target.set_screen_size(width, height);
        });
        let capturer: Box<dyn FrameCapturer> = Box::new(shm_capturer);
        let encoder: Box<dyn VideoEncoder> =
            Box::new(crate::encoder::RawVideoEncoder::new(encoder_config));
        let injector: Arc<dyn HostInputInjector> = uinput;
        Ok((
            capturer,
            encoder,
            injector,
            Some(screen_sink),
            Some(capture_stop),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    fn build_live_media(_encoder_config: EncoderConfig) -> Result<SessionMedia> {
        anyhow::bail!("live media is only implemented on Linux")
    }

    /// Linux follow-seat media: a capture worker per active seat0 session, raw frames, uinput.
    #[cfg(target_os = "linux")]
    fn build_follow_seat_media(encoder_config: EncoderConfig) -> Result<SessionMedia> {
        let command = crate::backends::WorkerCommand::current()?;
        let worker = crate::backends::WorkerCapturer::new(command, 0);
        let stopper = worker.stopper();
        let capture_stop: CaptureStop = Arc::new(move || stopper.stop());
        let capturer: Box<dyn FrameCapturer> = Box::new(worker);
        let uinput = Arc::new(
            crate::backends::UinputInjector::new(1920, 1080)
                .context("failed to create the /dev/uinput devices")?,
        );
        let sink_target = uinput.clone();
        let screen_sink: ScreenSink = Arc::new(move |width, height| {
            sink_target.set_screen_size(width, height);
        });
        let encoder: Box<dyn VideoEncoder> =
            Box::new(crate::encoder::RawVideoEncoder::new(encoder_config));
        let injector: Arc<dyn HostInputInjector> = uinput;
        Ok((
            capturer,
            encoder,
            injector,
            Some(screen_sink),
            Some(capture_stop),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    fn build_follow_seat_media(_encoder_config: EncoderConfig) -> Result<SessionMedia> {
        anyhow::bail!("follow-seat media is only implemented on Linux")
    }

    /// Returns `true` if the session capture engine is currently active.
    pub fn is_running(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    /// Returns a [`watch::Receiver`] that resolves to `true` when [`HostSessionEngine::stop`] is invoked.
    pub fn subscribe_stop(&self) -> watch::Receiver<bool> {
        self.stop_tx.subscribe()
    }

    /// Spawns the main GPU frame capture loop and 20 ms Opus audio loopback stream.
    pub async fn run_capture_loop(&self, outgoing_tx: mpsc::Sender<WireMessage>) -> Result<()> {
        self.run_capture_loop_with_video(outgoing_tx.clone(), outgoing_tx)
            .await
    }

    /// Spawns the main GPU frame capture loop (sending video frames to `video_tx` with
    /// non-blocking drop on backpressure) and the 20 ms Opus audio loopback stream on
    /// `outgoing_tx`.
    pub async fn run_capture_loop_with_video(
        &self,
        outgoing_tx: mpsc::Sender<WireMessage>,
        video_tx: mpsc::Sender<WireMessage>,
    ) -> Result<()> {
        info!(
            session_id = %self.session_id,
            backend = ?self.backend_type,
            "Starting host frame capture loop"
        );

        let session_id = self.session_id.clone();
        let is_active = self.is_active.clone();
        let stop_tx = self.stop_tx.clone();
        let mut stop_rx = self.subscribe_stop();
        let capturer = self.capturer.clone();
        let encoder = self.encoder.clone();
        let force_keyframe = self.force_keyframe_flag.clone();
        let frame_interval = self.frame_interval;
        let screen_sink = self.screen_sink.clone();

        tokio::spawn(async move {
            let target_frame_duration = frame_interval;
            let mut sent_size = (0u32, 0u32);

            while is_active.load(Ordering::Relaxed) && !*stop_rx.borrow() {
                let loop_start = Instant::now();

                let capture_task = {
                    let capturer = capturer.clone();
                    tokio::task::spawn_blocking(move || {
                        let started = Instant::now();
                        let frame = capturer.blocking_lock().capture_next_frame();
                        (frame, started.elapsed())
                    })
                };
                let (captured, capture_time) = tokio::select! {
                    joined = capture_task => match joined {
                        Ok(result) => result,
                        Err(e) => {
                            error!(session_id = %session_id, error = %e, "Capture thread failed; stopping session");
                            is_active.store(false, Ordering::SeqCst);
                            let _ = stop_tx.send(true);
                            break;
                        }
                    },
                    _ = stop_rx.changed() => break,
                };
                let captured_frame = match captured {
                    Ok(frame) => frame,
                    Err(e) => {
                        error!(
                            session_id = %session_id,
                            error = %format!("{e:#}"),
                            "Platform FrameCapturer capture_next_frame failed; stopping session"
                        );
                        is_active.store(false, Ordering::SeqCst);
                        let _ = stop_tx.send(true);
                        break;
                    }
                };

                let need_keyframe = force_keyframe.swap(false, Ordering::SeqCst);

                let encode_started = Instant::now();
                let encoded = encoder.lock().await.encode(&captured_frame, need_keyframe);
                let encode_time = encode_started.elapsed();
                match encoded {
                    Ok(video_msg) => {
                        if let Some(sink) = &screen_sink
                            && (video_msg.width, video_msg.height) != sent_size
                        {
                            sent_size = (video_msg.width, video_msg.height);
                            sink(sent_size.0, sent_size.1);
                        }
                        let payload_bytes = video_msg.payload.len();
                        let enqueue_started = Instant::now();
                        match video_tx.try_send(WireMessage::Video(video_msg)) {
                            Ok(()) => {
                                debug!(
                                    session_id = %session_id,
                                    capture_ms = capture_time.as_millis() as u64,
                                    encode_ms = encode_time.as_millis() as u64,
                                    enqueue_ms = enqueue_started.elapsed().as_millis() as u64,
                                    payload_bytes,
                                    "Frame timing"
                                );
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                if need_keyframe {
                                    force_keyframe.store(true, Ordering::SeqCst);
                                }
                                debug!(
                                    session_id = %session_id,
                                    "Dropping video frame; outbound video queue full"
                                );
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                error!("Outgoing transport channel closed; exiting capture loop");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Video frame encoding failed");
                    }
                }

                let elapsed = loop_start.elapsed();
                if elapsed < target_frame_duration {
                    tokio::select! {
                        _ = tokio::time::sleep(target_frame_duration - elapsed) => {}
                        _ = stop_rx.changed() => break,
                    }
                }
            }
        });

        self.audio_capturer
            .start_loopback_capture(outgoing_tx)
            .await?;

        Ok(())
    }

    /// Handles incoming wire messages from the remote client (input injection, adaptive control, and virtual channels).
    pub async fn handle_incoming_wire_message(&self, msg: WireMessage) -> Result<()> {
        match msg {
            WireMessage::Input(input_event) => {
                self.input_injector.inject_input(&input_event)?;
            }
            WireMessage::Control(ctrl) => match ctrl {
                ControlMessage::FrameAck {
                    frame_id,
                    monitor_id,
                    render_delay_us,
                } => {
                    self.last_acked_frame_id.store(frame_id, Ordering::SeqCst);
                    debug!(
                        frame_id,
                        monitor_id, render_delay_us, "Processed client FrameAck"
                    );
                }
                ControlMessage::LossReport { missing_frames } => {
                    warn!(
                        missing = missing_frames.len(),
                        first = ?missing_frames.first(),
                        last = ?missing_frames.last(),
                        "Client reported packet loss; triggering instant keyframe"
                    );
                    self.force_keyframe_flag.store(true, Ordering::SeqCst);
                }
                ControlMessage::BandwidthEstimate {
                    bytes_per_sec,
                    rtt_ms,
                    ..
                } => {
                    let bitrate_kbps = bitrate_kbps_from_bytes_per_sec(bytes_per_sec);
                    debug!(
                        bitrate_kbps,
                        rtt_ms, "Updating encoder bitrate from network feedback"
                    );
                    let mut enc = self.encoder.lock().await;
                    let _ = enc.update_bitrate(bitrate_kbps);
                }
                ControlMessage::Disconnect { reason } => {
                    info!(
                        session_id = %self.session_id,
                        %reason,
                        "Client requested session disconnect"
                    );
                    self.stop();
                }
                _ => {}
            },
            WireMessage::Channel(channel_msg) => {
                self.channel_manager
                    .route_channel_message(channel_msg)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Stops both the video frame capture loop and the audio loopback capture task.
    pub fn stop(&self) {
        self.is_active.store(false, Ordering::SeqCst);
        let _ = self.stop_tx.send(true);
        if let Some(capture_stop) = &self.capture_stop {
            capture_stop();
        }
        self.audio_capturer.stop();
        info!(session_id = %self.session_id, "Host session stopped");
    }
}

fn bitrate_kbps_from_bytes_per_sec(bytes_per_sec: u64) -> u32 {
    u32::try_from(bytes_per_sec.saturating_mul(8) / 1000).unwrap_or(u32::MAX)
}

/// How long a newly connected client has to send its `ClientHello` before the host closes the
/// connection.
pub const CLIENT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Elevated Host Daemon Service Background Manager (`systemd`, Windows Service, macOS `launchd`)
pub struct PerigeeHostDaemon {
    pub host_name: String,
    pub backend: CaptureBackendType,
    pub runtime_env: RuntimeEnvironment,
    pub active_sessions: Arc<Mutex<Vec<Arc<HostSessionEngine>>>>,
    pub client_hello_timeout: Duration,
    /// Media backends for new sessions; [`MediaMode::StandIn`] unless `bind_host` sets it.
    pub media: MediaMode,
}

impl PerigeeHostDaemon {
    /// Creates a new [`PerigeeHostDaemon`].
    pub fn new(host_name: String, backend: CaptureBackendType) -> Self {
        let runtime_env = RuntimeEnvironment::detect();
        info!(
            host_name = %host_name,
            backend = ?backend,
            platform = ?runtime_env.platform,
            is_cosmic = runtime_env.is_cosmic_desktop,
            "Initializing elevated Perigee Host Daemon Service"
        );
        Self {
            host_name,
            backend,
            runtime_env,
            active_sessions: Arc::new(Mutex::new(Vec::new())),
            client_hello_timeout: CLIENT_HELLO_TIMEOUT,
            media: MediaMode::StandIn,
        }
    }

    /// Auto-detects operating system backend (Wayland/COSMIC, X11 fallback, DXGI, or ScreenCaptureKit).
    pub fn auto_detect_backend() -> CaptureBackendType {
        if cfg!(target_os = "macos") {
            CaptureBackendType::MacOsScreenCaptureKit
        } else if cfg!(target_os = "windows") {
            CaptureBackendType::WindowsDxgi
        } else {
            let env = RuntimeEnvironment::detect();
            if env.is_wayland || env.is_cosmic_desktop {
                CaptureBackendType::WaylandPipeWire
            } else if std::env::var("DISPLAY").is_ok() {
                CaptureBackendType::X11Fallback
            } else {
                CaptureBackendType::WaylandPipeWire
            }
        }
    }

    /// Validates a client's opening `ClientHello` and returns a new session ID. The peer has
    /// already been authenticated by mutual TLS before this runs.
    pub async fn handle_handshake(&self, handshake: HandshakeMessage) -> Result<String> {
        info!(
            client_hello = ?handshake,
            "Processing incoming client session handshake"
        );

        match handshake {
            HandshakeMessage::ClientHello {
                protocol_version,
                client_id,
                supported_codecs,
                ..
            } => {
                if protocol_version == 0 {
                    bail!("Unsupported Perigee protocol version 0");
                }
                if client_id.trim().is_empty() {
                    bail!("ClientHello client_id must not be empty");
                }
                if supported_codecs.is_empty() {
                    bail!("ClientHello must advertise at least one VideoCodec");
                }
                Ok(uuid::Uuid::new_v4().to_string())
            }
            _ => bail!("Expected ClientHello as the first session handshake message"),
        }
    }

    /// Provisions and registers a new [`HostSessionEngine`] in `active_sessions`.
    pub async fn provision_session(
        &self,
        session_id: String,
        encoder_config: EncoderConfig,
    ) -> Result<Arc<HostSessionEngine>> {
        let session = Arc::new(HostSessionEngine::with_media(
            session_id,
            self.backend,
            encoder_config,
            self.media,
        )?);
        let mut sessions = self.active_sessions.lock().await;
        sessions.push(session.clone());
        Ok(session)
    }

    /// Looks up an active [`HostSessionEngine`] by `session_id`.
    pub async fn get_session(&self, session_id: &str) -> Option<Arc<HostSessionEngine>> {
        let sessions = self.active_sessions.lock().await;
        sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
    }

    /// Stops and removes the session matching `session_id`, returning `true` if found.
    pub async fn terminate_session(&self, session_id: &str) -> bool {
        let mut sessions = self.active_sessions.lock().await;
        if let Some(pos) = sessions.iter().position(|s| s.session_id == session_id) {
            let removed = sessions.remove(pos);
            removed.stop();
            true
        } else {
            false
        }
    }

    /// Returns the number of currently registered active sessions.
    pub async fn active_session_count(&self) -> usize {
        self.active_sessions.lock().await.len()
    }

    /// Stops and clears all active sessions managed by the daemon.
    pub async fn shutdown_all_sessions(&self) {
        let mut sessions = self.active_sessions.lock().await;
        for session in sessions.drain(..) {
            session.stop();
        }
    }

    async fn send_auth_failure_and_retain(conn: Arc<QuicConnectionManager>, error_message: String) {
        let _ = conn
            .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                success: false,
                session_token: None,
                error_message: Some(error_message),
            }))
            .await;
        // Retain `conn` briefly so `quinn::Connection::drop` does not transmit
        // `CONNECTION_CLOSE` before the peer reads the finished `AuthResponse` stream.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(conn);
        });
    }

    /// Serves an incoming [`QuicConnectionManager`] connection:
    /// 1. Validates the client's `ClientHello` (the peer is already authenticated by mutual TLS)
    ///    and replies with [`HandshakeMessage::AuthResponse`].
    /// 2. Provisions a new [`HostSessionEngine`] and [`VirtualChannelManager`].
    /// 3. Starts [`HostSessionEngine::run_capture_loop`] and pumps outbound video/audio frames over
    ///    QUIC streams.
    /// 4. Routes inbound QUIC datagrams, unidirectional streams, and bidirectional streams to
    ///    [`HostSessionEngine::handle_incoming_wire_message`] and [`VirtualChannelManager`].
    /// 5. Removes the session from the registry and closes the connection when the session stops or the peer disconnects.
    pub async fn serve_connection(
        &self,
        conn: impl Into<Arc<QuicConnectionManager>>,
    ) -> Result<Arc<HostSessionEngine>> {
        let conn: Arc<QuicConnectionManager> = conn.into();

        let initial_wire =
            match tokio::time::timeout(self.client_hello_timeout, conn.recv_uni_message()).await {
                Ok(result) => result
                    .context("Failed to receive initial handshake message over QUIC stream")?,
                Err(_) => {
                    conn.connection.close(0u32.into(), b"no ClientHello");
                    bail!(
                        "client sent no ClientHello within {:?}",
                        self.client_hello_timeout
                    );
                }
            };

        let WireMessage::Handshake(handshake) = initial_wire else {
            bail!(
                "Expected initial WireMessage::Handshake on QUIC connection, got {initial_wire:?}"
            );
        };

        let preferred_codec = match &handshake {
            HandshakeMessage::ClientHello {
                supported_codecs, ..
            } => supported_codecs.first().copied().unwrap_or(VideoCodec::AV1),
            _ => VideoCodec::AV1,
        };

        let session_id = match self.handle_handshake(handshake).await {
            Ok(id) => id,
            Err(e) => {
                Self::send_auth_failure_and_retain(conn, format!("{e:#}")).await;
                return Err(e);
            }
        };
        info!(
            session_id = %session_id,
            remote = %conn.connection.remote_address(),
            peer_node_key = %conn
                .peer_node_key()
                .map(|key| node_key_to_hex(&key))
                .unwrap_or_else(|| "missing".to_string()),
            "Admitted client session"
        );

        let encoder_config = EncoderConfig {
            codec: preferred_codec,
            ..EncoderConfig::default()
        };
        let session = match self
            .provision_session(session_id.clone(), encoder_config)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                Self::send_auth_failure_and_retain(conn, format!("{e:#}")).await;
                return Err(e);
            }
        };

        if let Err(e) = conn
            .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                success: true,
                session_token: Some(session_id.clone()),
                error_message: None,
            }))
            .await
        {
            self.terminate_session(&session_id).await;
            return Err(e.context("Failed to send HandshakeMessage::AuthResponse over QUIC stream"));
        }

        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<WireMessage>(256);
        let (video_tx, mut video_rx) = mpsc::channel::<WireMessage>(MAX_OUTBOUND_VIDEO_BACKLOG);
        if let Err(e) = session
            .run_capture_loop_with_video(outgoing_tx, video_tx)
            .await
        {
            self.terminate_session(&session_id).await;
            return Err(e);
        }

        // Task 1: Pump outbound video/audio/channel frames over QUIC streams
        let conn_outbound = conn.clone();
        let session_outbound = session.clone();
        let mut stop_outbound = session.subscribe_stop();
        tokio::spawn(async move {
            while session_outbound.is_running() && !*stop_outbound.borrow() {
                let maybe_msg = tokio::select! {
                    msg = outgoing_rx.recv() => msg,
                    msg = video_rx.recv() => msg,
                    _ = stop_outbound.changed() => None,
                };
                let Some(msg) = maybe_msg else {
                    break;
                };
                if !session_outbound.is_running() {
                    break;
                }
                let res = match &msg {
                    WireMessage::Input(_)
                    | WireMessage::Control(ControlMessage::FrameAck { .. }) => {
                        conn_outbound.send_datagram(&msg)
                    }
                    WireMessage::Video(video) => {
                        let send_started = Instant::now();
                        let res = conn_outbound.send_uni_message(&msg).await;
                        if res.is_ok() {
                            debug!(
                                session_id = %session_outbound.session_id,
                                frame_id = video.frame_id,
                                payload_bytes = video.payload.len(),
                                net_send_ms = send_started.elapsed().as_millis() as u64,
                                "Video frame sent"
                            );
                        }
                        res
                    }
                    _ => conn_outbound.send_uni_message(&msg).await,
                };
                if let Err(e) = res {
                    debug!(
                        session_id = %session_outbound.session_id,
                        error = %e,
                        "Outbound QUIC stream pump terminated"
                    );
                    session_outbound.stop();
                    break;
                }
            }
        });

        // Task 2: Route inbound QUIC datagrams to handle_incoming_wire_message and VirtualChannelManager
        let conn_dgram = conn.clone();
        let session_dgram = session.clone();
        let channel_mgr_dgram = session.channel_manager.clone();
        let mut stop_dgram = session.subscribe_stop();
        tokio::spawn(async move {
            while session_dgram.is_running() && !*stop_dgram.borrow() {
                let recv_res = tokio::select! {
                    res = conn_dgram.recv_datagram() => res,
                    _ = stop_dgram.changed() => break,
                };
                match recv_res {
                    Ok(WireMessage::Channel(vc)) => {
                        if let Err(e) = channel_mgr_dgram.route_channel_message(vc).await {
                            warn!(
                                session_id = %session_dgram.session_id,
                                error = %e,
                                "Failed to route inbound datagram virtual channel message"
                            );
                        }
                    }
                    Ok(msg) => {
                        if let Err(e) = session_dgram.handle_incoming_wire_message(msg).await {
                            warn!(
                                session_id = %session_dgram.session_id,
                                error = %e,
                                "Failed to process inbound QUIC datagram message"
                            );
                        }
                    }
                    Err(e) => {
                        if conn_dgram.connection.close_reason().is_some() {
                            debug!(
                                session_id = %session_dgram.session_id,
                                error = %e,
                                "Inbound QUIC datagram receiver loop ended"
                            );
                            session_dgram.stop();
                            break;
                        }
                        debug!(
                            session_id = %session_dgram.session_id,
                            error = %e,
                            "Failed to decode inbound QUIC datagram message"
                        );
                    }
                }
            }
        });

        // Task 3: Route inbound QUIC unidirectional streams to handle_incoming_wire_message and VirtualChannelManager
        let conn_uni = conn.clone();
        let session_uni = session.clone();
        let channel_mgr_uni = session.channel_manager.clone();
        let mut stop_uni = session.subscribe_stop();
        tokio::spawn(async move {
            while session_uni.is_running() && !*stop_uni.borrow() {
                let recv_res = tokio::select! {
                    res = conn_uni.recv_uni_message() => res,
                    _ = stop_uni.changed() => break,
                };
                match recv_res {
                    Ok(WireMessage::Channel(vc)) => {
                        if let Err(e) = channel_mgr_uni.route_channel_message(vc).await {
                            warn!(
                                session_id = %session_uni.session_id,
                                error = %e,
                                "Failed to route inbound stream virtual channel message"
                            );
                        }
                    }
                    Ok(msg) => {
                        if let Err(e) = session_uni.handle_incoming_wire_message(msg).await {
                            warn!(
                                session_id = %session_uni.session_id,
                                error = %e,
                                "Failed to process inbound QUIC unidirectional stream message"
                            );
                        }
                    }
                    Err(e) => {
                        debug!(
                            session_id = %session_uni.session_id,
                            error = %e,
                            "Inbound QUIC unidirectional stream receiver loop ended"
                        );
                        session_uni.stop();
                        break;
                    }
                }
            }
        });

        // Task 4: Route inbound QUIC bidirectional streams (e.g., SimplyTransfer / Clipboard)
        let conn_bi = conn.clone();
        let session_bi = session.clone();
        let channel_mgr_bi = session.channel_manager.clone();
        let mut stop_bi = session.subscribe_stop();
        tokio::spawn(async move {
            while session_bi.is_running() && !*stop_bi.borrow() {
                let accept_res = tokio::select! {
                    res = conn_bi.connection.accept_bi() => res,
                    _ = stop_bi.changed() => break,
                };
                let (mut send_stream, mut recv_stream) = match accept_res {
                    Ok(streams) => streams,
                    Err(e) => {
                        if conn_bi.connection.close_reason().is_some() {
                            debug!(
                                session_id = %session_bi.session_id,
                                error = %e,
                                "Inbound QUIC bidirectional stream receiver loop ended"
                            );
                            session_bi.stop();
                            break;
                        }
                        debug!(
                            session_id = %session_bi.session_id,
                            error = %e,
                            "Failed to accept inbound QUIC bidirectional stream"
                        );
                        continue;
                    }
                };
                let data = match recv_stream
                    .read_to_end(perigee_protocol::MAX_WIRE_FRAME_BYTES + 4)
                    .await
                {
                    Ok(data) => data,
                    Err(e) => {
                        if conn_bi.connection.close_reason().is_some() {
                            debug!(
                                session_id = %session_bi.session_id,
                                error = %e,
                                "Inbound QUIC bidirectional stream read ended due to connection close"
                            );
                            session_bi.stop();
                            break;
                        }
                        debug!(
                            session_id = %session_bi.session_id,
                            error = %e,
                            "Failed to read inbound QUIC bidirectional stream"
                        );
                        continue;
                    }
                };
                conn_bi
                    .metrics
                    .bytes_received
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                match decode_length_prefixed(&data) {
                    Ok(WireMessage::Channel(vc)) => {
                        if let Err(e) = channel_mgr_bi.route_channel_message(vc).await {
                            warn!(
                                session_id = %session_bi.session_id,
                                error = %e,
                                "Failed to route inbound bidirectional stream virtual channel message"
                            );
                        } else {
                            let _ = send_stream.finish();
                        }
                    }
                    Ok(msg) => {
                        if let Err(e) = session_bi.handle_incoming_wire_message(msg).await {
                            warn!(
                                session_id = %session_bi.session_id,
                                error = %e,
                                "Failed to process inbound QUIC bidirectional stream message"
                            );
                        } else {
                            let _ = send_stream.finish();
                        }
                    }
                    Err(e) => {
                        if conn_bi.connection.close_reason().is_some() {
                            session_bi.stop();
                            break;
                        }
                        debug!(
                            session_id = %session_bi.session_id,
                            error = %e,
                            "Failed to decode inbound QUIC bidirectional stream message"
                        );
                    }
                }
            }
        });

        let active_sessions_reaper = Arc::clone(&self.active_sessions);
        let session_reaper = session.clone();
        let mut stop_reaper = session.subscribe_stop();
        let conn_reaper = conn;
        tokio::spawn(async move {
            let ended_by = tokio::select! {
                _ = stop_reaper.wait_for(|&stopped| stopped) => "host session stop",
                _ = conn_reaper.connection.closed() => "peer connection close",
            };
            session_reaper.stop();
            {
                let mut sessions = active_sessions_reaper.lock().await;
                sessions.retain(|s| s.session_id != session_reaper.session_id);
            }
            conn_reaper.connection.close(0u32.into(), b"session ended");
            info!(
                session_id = %session_reaper.session_id,
                ended_by,
                "Reaped stopped session and closed QUIC connection"
            );
        });

        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapturedFrame;
    use anyhow::anyhow;
    use perigee_channels::ClipboardContent;
    use perigee_net::{
        LoopbackPeers, QuicClientTransport, QuicServerListener, QuicTransportConfig,
    };
    use perigee_protocol::{
        AudioCodec, AudioDirection, ClipboardPayload, ElementState, InputEvent, MonitorInfo,
        MouseButtonCode, VirtualChannelMessage, encode_length_prefixed,
    };

    struct FailingCapturer;

    impl FrameCapturer for FailingCapturer {
        fn backend_type(&self) -> CaptureBackendType {
            CaptureBackendType::WaylandPipeWire
        }

        fn is_live_display(&self) -> bool {
            false
        }

        fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
            Ok(vec![])
        }

        fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
            Err(anyhow!("boom"))
        }
    }

    #[tokio::test]
    async fn test_capture_error_stops_session() {
        let session = HostSessionEngine::new(
            "failing-cap-test".to_string(),
            CaptureBackendType::WaylandPipeWire,
            EncoderConfig::default(),
        )
        .expect("HostSessionEngine::new should succeed");
        *session.capturer.lock().await = Box::new(FailingCapturer);

        let (tx, mut rx) = mpsc::channel(16);
        session.run_capture_loop(tx).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            while session.is_running() || !*session.subscribe_stop().borrow() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for failing capture to stop session");

        assert!(!session.is_running());
        assert!(*session.subscribe_stop().borrow());
        session.stop();

        while let Ok(msg) = rx.try_recv() {
            if let WireMessage::Video(v) = msg {
                assert!(
                    !(v.width == 2560 && v.height == 1440),
                    "Should not receive a fallback 2560x1440 video frame from failing capturer"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_run_capture_loop_across_platform_frame_capturers() {
        let backends = [
            CaptureBackendType::WaylandPipeWire,
            CaptureBackendType::X11Fallback,
            CaptureBackendType::MacOsScreenCaptureKit,
            CaptureBackendType::WindowsDxgi,
        ];

        for backend in backends {
            let engine = HostSessionEngine::new(
                format!("cap-test-{backend:?}"),
                backend,
                EncoderConfig {
                    width: 1920,
                    height: 1080,
                    ..EncoderConfig::default()
                },
            )
            .expect("HostSessionEngine::new should succeed");
            let (actual_backend, monitors) = {
                let cap = engine.capturer.lock().await;
                (cap.backend_type(), cap.enumerate_monitors().unwrap())
            };
            assert_eq!(actual_backend, backend);
            assert!(monitors[0].width >= 1);
            assert!(monitors[0].height >= 1);

            let (tx, mut rx) = mpsc::channel(16);
            engine.run_capture_loop(tx).await.unwrap();

            let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("Timed out waiting for captured frame")
                .expect("Capture channel closed");

            match msg {
                WireMessage::Video(v) => {
                    assert_eq!(v.width, monitors[0].width);
                    assert_eq!(v.height, monitors[0].height);
                    assert!(v.is_keyframe);
                }
                WireMessage::Audio(a) => {
                    assert_eq!(a.codec, AudioCodec::Opus);
                }
                other => panic!("Unexpected wire message from capture loop: {other:?}"),
            }

            engine.stop();
            assert!(!engine.is_running());
        }
    }

    #[tokio::test]
    async fn test_serve_connection_over_loopback_quic() {
        let peers = LoopbackPeers::generate().unwrap();
        let client_key = peers.client.public_key();
        let daemon = Arc::new(PerigeeHostDaemon::new(
            "loopback-host-daemon".to_string(),
            CaptureBackendType::WaylandPipeWire,
        ));

        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();
        let expected_cert_sha256 = listener.server_cert_sha256;

        let daemon_server = daemon.clone();
        let server_task = tokio::spawn(async move {
            let server_conn = listener
                .accept()
                .await
                .expect("Server failed to accept QUIC connection");
            assert_eq!(server_conn.peer_node_key(), Some(client_key));
            let session = daemon_server
                .serve_connection(server_conn)
                .await
                .expect("serve_connection should succeed for a valid ClientHello");
            (listener, session)
        });

        let client = QuicClientTransport::new(QuicTransportConfig::default());
        let client_conn = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .expect("Client failed to connect to QUIC listener");

        let peer_cert_sha256 = client_conn
            .peer_cert_sha256()
            .expect("Client should capture server TLS 1.3 leaf cert SHA-256");
        assert_eq!(peer_cert_sha256, expected_cert_sha256);

        // 1. Send ClientHello (the connection is already authenticated by mutual TLS)
        let hello = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "loopback-quic-client".to_string(),
            client_name: "Loopback Viewer".to_string(),
            supported_codecs: vec![VideoCodec::AV1],
        };

        client_conn
            .send_uni_message(&WireMessage::Handshake(hello))
            .await
            .expect("Client failed to send ClientHello");

        // 2. Receive AuthResponse from serve_connection
        let auth_resp_wire =
            tokio::time::timeout(Duration::from_secs(5), client_conn.recv_uni_message())
                .await
                .expect("Timed out waiting for AuthResponse")
                .expect("Failed to receive AuthResponse");
        let WireMessage::Handshake(HandshakeMessage::AuthResponse {
            success: true,
            session_token: Some(session_token),
            ..
        }) = auth_resp_wire
        else {
            panic!("Expected successful AuthResponse, got {auth_resp_wire:?}");
        };
        assert!(!session_token.is_empty());

        let (_listener, session) = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("Server task timed out")
            .expect("Server task panicked");
        assert_eq!(session.session_id, session_token);
        assert_eq!(daemon.active_session_count().await, 1);

        // 3. Receive outbound Video and Audio frames pumped over QUIC unidirectional streams
        let mut saw_video = false;
        let mut saw_audio = false;
        for _ in 0..16 {
            let msg = tokio::time::timeout(Duration::from_secs(5), client_conn.recv_uni_message())
                .await
                .expect("Timed out waiting for outbound media stream")
                .expect("Failed to read outbound media stream");
            match msg {
                WireMessage::Video(v) => {
                    assert_eq!(v.width, 2560);
                    assert_eq!(v.height, 1440);
                    saw_video = true;
                }
                WireMessage::Audio(a) => {
                    assert_eq!(a.codec, AudioCodec::Opus);
                    assert_eq!(a.direction, AudioDirection::HostToClientSpeaker);
                    saw_audio = true;
                }
                _ => {}
            }
            if saw_video && saw_audio {
                break;
            }
        }
        assert!(
            saw_video,
            "Expected outbound VideoFrameMessage over QUIC stream"
        );
        assert!(
            saw_audio,
            "Expected outbound AudioFrameMessage over QUIC stream"
        );

        // 4. Send inbound QUIC datagrams (InputEvent + FrameAck) and verify routing to HostSessionEngine
        client_conn
            .send_datagram(&WireMessage::Input(InputEvent::MouseButton {
                button: MouseButtonCode::Left,
                state: ElementState::Pressed,
                x: 640.0,
                y: 360.0,
            }))
            .expect("Failed to send InputEvent datagram");
        client_conn
            .send_datagram(&WireMessage::Control(ControlMessage::FrameAck {
                frame_id: 77,
                monitor_id: 1,
                render_delay_us: 250,
            }))
            .expect("Failed to send FrameAck datagram");

        // 5. Send inbound VirtualChannelMessage over QUIC bi stream
        let (mut bi_send, _bi_recv) = client_conn
            .open_bidirectional_stream()
            .await
            .expect("Failed to open bidirectional stream");
        let bi_clipboard = WireMessage::Channel(VirtualChannelMessage::Clipboard(
            ClipboardPayload::Files(vec!["/tmp/perigee-test/archive.tar.zst".to_string()]),
        ));
        let framed_bi = encode_length_prefixed(&bi_clipboard).unwrap();
        bi_send.write_all(&framed_bi).await.unwrap();
        bi_send.finish().unwrap();

        // Poll briefly for async inbound datagram, uni-stream, and bi-stream handlers to process messages
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let acked = session.last_acked_frame_id.load(Ordering::SeqCst) == 77;
                let clipboard_matched = session
                    .channel_manager
                    .clipboard_handler
                    .last_content()
                    .await
                    == ClipboardContent::Files(vec![
                        "/tmp/perigee-test/archive.tar.zst".to_string(),
                    ]);
                if acked && clipboard_matched {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for inbound datagrams and virtual channel messages to route");

        // 6. Send Disconnect control message over uni stream and verify session stops
        client_conn
            .send_uni_message(&WireMessage::Control(ControlMessage::Disconnect {
                reason: "Loopback test complete".to_string(),
            }))
            .await
            .expect("Failed to send Disconnect message");

        tokio::time::timeout(Duration::from_secs(5), async {
            while session.is_running() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for session to stop after Disconnect");

        assert!(!session.is_running());
        tokio::time::timeout(Duration::from_secs(5), async {
            while daemon.active_session_count().await != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for session to be removed from registry");
        assert_eq!(daemon.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn test_session_removed_when_client_connection_closes() {
        let peers = LoopbackPeers::generate().unwrap();
        let daemon = Arc::new(PerigeeHostDaemon::new(
            "client-close-host-daemon".to_string(),
            CaptureBackendType::WaylandPipeWire,
        ));

        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();
        let expected_cert_sha256 = listener.server_cert_sha256;

        let daemon_server = daemon.clone();
        let server_task = tokio::spawn(async move {
            let server_conn = listener
                .accept()
                .await
                .expect("Server failed to accept QUIC connection");
            let session = daemon_server
                .serve_connection(server_conn)
                .await
                .expect("serve_connection should succeed for valid Ed25519 handshake");
            (listener, session)
        });

        let client = QuicClientTransport::new(QuicTransportConfig::default());
        let client_conn = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .expect("Client failed to connect to QUIC listener");

        let peer_cert_sha256 = client_conn
            .peer_cert_sha256()
            .expect("Client should capture server TLS 1.3 leaf cert SHA-256");
        assert_eq!(peer_cert_sha256, expected_cert_sha256);

        let hello = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "client-close-quic-client".to_string(),
            client_name: "Loopback Viewer".to_string(),
            supported_codecs: vec![VideoCodec::AV1],
        };

        client_conn
            .send_uni_message(&WireMessage::Handshake(hello))
            .await
            .expect("Client failed to send ClientHello");

        let auth_resp_wire =
            tokio::time::timeout(Duration::from_secs(5), client_conn.recv_uni_message())
                .await
                .expect("Timed out waiting for AuthResponse")
                .expect("Failed to receive AuthResponse");
        let WireMessage::Handshake(HandshakeMessage::AuthResponse {
            success: true,
            session_token: Some(session_token),
            ..
        }) = auth_resp_wire
        else {
            panic!("Expected successful AuthResponse, got {auth_resp_wire:?}");
        };
        assert!(!session_token.is_empty());

        let (_listener, session) = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("Server task timed out")
            .expect("Server task panicked");
        assert_eq!(session.session_id, session_token);
        assert_eq!(daemon.active_session_count().await, 1);

        client_conn.connection.close(0u32.into(), b"bye");

        tokio::time::timeout(Duration::from_secs(5), async {
            while daemon.active_session_count().await != 0 || session.is_running() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for session removal and stop after client close");

        assert_eq!(daemon.active_session_count().await, 0);
        assert!(!session.is_running());
    }

    #[tokio::test]
    async fn test_serve_connection_rejects_non_hello_and_isolates_concurrent_sessions() {
        let peers = LoopbackPeers::generate().unwrap();
        let daemon = Arc::new(PerigeeHostDaemon::new(
            "concurrent-host-daemon".to_string(),
            CaptureBackendType::WaylandPipeWire,
        ));

        let listener = Arc::new(
            QuicServerListener::bind_mtls(
                "127.0.0.1:0".parse().unwrap(),
                QuicTransportConfig::default(),
                peers.server_auth(),
            )
            .expect("Failed to bind QUIC listener"),
        );
        let server_addr = listener.local_addr().unwrap();

        // A first handshake message other than ClientHello gets AuthResponse { success: false }
        let daemon_server = daemon.clone();
        let listener_bad = listener.clone();
        let server_task = tokio::spawn(async move {
            let server_conn = listener_bad.accept().await.unwrap();
            daemon_server.serve_connection(server_conn).await
        });

        let client = QuicClientTransport::new(QuicTransportConfig::default());
        let client_conn = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .expect("Client failed to connect");

        client_conn
            .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                success: true,
                session_token: None,
                error_message: None,
            }))
            .await
            .unwrap();

        let resp = tokio::time::timeout(Duration::from_secs(5), client_conn.recv_uni_message())
            .await
            .expect("Timed out waiting for failure AuthResponse")
            .expect("Failed to read failure AuthResponse");
        assert!(matches!(
            resp,
            WireMessage::Handshake(HandshakeMessage::AuthResponse { success: false, .. })
        ));

        let srv_res = server_task.await.unwrap();
        assert!(
            srv_res.is_err(),
            "serve_connection must refuse a first message that is not ClientHello"
        );
        assert_eq!(daemon.active_session_count().await, 0);

        // Verify two concurrent clients can connect and disconnecting one does not disrupt the surviving session
        let daemon_c1 = daemon.clone();
        let listener_c1 = listener.clone();
        let accept_two = tokio::spawn(async move {
            let conn1 = listener_c1.accept().await.unwrap();
            let s1 = daemon_c1.serve_connection(conn1).await.unwrap();
            let conn2 = listener_c1.accept().await.unwrap();
            let s2 = daemon_c1.serve_connection(conn2).await.unwrap();
            (s1, s2)
        });

        let c1 = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .unwrap();
        let hello1 = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "concurrent-client-1".to_string(),
            client_name: "Viewer 1".to_string(),
            supported_codecs: vec![VideoCodec::AV1],
        };
        c1.send_uni_message(&WireMessage::Handshake(hello1))
            .await
            .unwrap();
        let _ = c1.recv_uni_message().await.unwrap();

        let c2 = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .unwrap();
        let hello2 = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "concurrent-client-2".to_string(),
            client_name: "Viewer 2".to_string(),
            supported_codecs: vec![VideoCodec::H265],
        };
        c2.send_uni_message(&WireMessage::Handshake(hello2))
            .await
            .unwrap();
        let _ = c2.recv_uni_message().await.unwrap();

        let (s1, s2) = tokio::time::timeout(Duration::from_secs(5), accept_two)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(daemon.active_session_count().await, 2);

        // Terminate session 1 and verify session 2 remains active and continues streaming
        assert!(daemon.terminate_session(&s1.session_id).await);
        assert!(!s1.is_running());
        assert!(s2.is_running());
        assert_eq!(daemon.active_session_count().await, 1);

        let c2_media = tokio::time::timeout(Duration::from_secs(5), c2.recv_uni_message())
            .await
            .expect("Surviving session 2 should continue streaming media")
            .expect("Failed to receive media on surviving session 2");
        assert!(matches!(
            c2_media,
            WireMessage::Video(_) | WireMessage::Audio(_)
        ));

        daemon.shutdown_all_sessions().await;
        assert!(!s2.is_running());
        assert_eq!(daemon.active_session_count().await, 0);
    }

    #[test]
    fn test_bitrate_kbps_saturates() {
        assert_eq!(bitrate_kbps_from_bytes_per_sec(0), 0);
        assert_eq!(bitrate_kbps_from_bytes_per_sec(125_000), 1000);
        assert_eq!(bitrate_kbps_from_bytes_per_sec(u64::MAX), u32::MAX);
    }

    #[tokio::test]
    async fn test_bandwidth_estimate_from_peer_does_not_panic() {
        let session = HostSessionEngine::new(
            "bw-estimate-test".to_string(),
            CaptureBackendType::WaylandPipeWire,
            EncoderConfig::default(),
        )
        .expect("HostSessionEngine::new should succeed");

        session
            .handle_incoming_wire_message(WireMessage::Control(ControlMessage::BandwidthEstimate {
                bytes_per_sec: u64::MAX,
                rtt_ms: 1.0,
                jitter_ms: 0.0,
            }))
            .await
            .expect("handle_incoming_wire_message should succeed");
    }

    #[tokio::test]
    async fn test_serve_connection_times_out_without_client_hello() {
        let peers = LoopbackPeers::generate().unwrap();
        let mut daemon = PerigeeHostDaemon::new(
            "hello-timeout-host".to_string(),
            CaptureBackendType::WaylandPipeWire,
        );
        daemon.client_hello_timeout = Duration::from_millis(200);
        let daemon = Arc::new(daemon);
        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();
        let daemon_server = daemon.clone();
        let server_task = tokio::spawn(async move {
            let server_conn = listener.accept().await.unwrap();
            let result = daemon_server.serve_connection(server_conn).await;
            (listener, result)
        });
        let client = QuicClientTransport::new(QuicTransportConfig::default());
        let client_conn = client
            .connect_mtls(server_addr, peers.client_auth())
            .await
            .expect("Client failed to connect");
        let (_listener, result) = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve_connection did not time out")
            .unwrap();
        let Err(err) = result else {
            panic!("serve_connection must fail without a ClientHello");
        };
        assert!(
            format!("{err:#}").contains("no ClientHello"),
            "got: {err:#}"
        );
        assert_eq!(daemon.active_session_count().await, 0);
        tokio::time::timeout(Duration::from_secs(5), client_conn.connection.closed())
            .await
            .expect("host did not close the connection");
    }

    struct BlockingCapturer {
        release: Arc<AtomicBool>,
    }

    impl FrameCapturer for BlockingCapturer {
        fn backend_type(&self) -> CaptureBackendType {
            CaptureBackendType::WaylandPipeWire
        }

        fn is_live_display(&self) -> bool {
            true
        }

        fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
            Ok(Vec::new())
        }

        fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            anyhow::bail!("released")
        }
    }

    #[tokio::test]
    async fn test_stop_ends_capture_loop_blocked_in_capture() {
        let session = HostSessionEngine::new(
            "blocked-capture".to_string(),
            CaptureBackendType::WaylandPipeWire,
            EncoderConfig::default(),
        )
        .unwrap();
        let release = Arc::new(AtomicBool::new(false));
        *session.capturer.lock().await = Box::new(BlockingCapturer {
            release: release.clone(),
        });
        let (tx, mut rx) = mpsc::channel(4);
        session.run_capture_loop(tx).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        session.stop();
        // The loop must not wait for the blocked capture: stop() alone ends the session.
        let closed = tokio::time::timeout(Duration::from_secs(2), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        release.store(true, Ordering::SeqCst);
        closed.expect("capture loop kept its sender after stop");
        assert!(!session.is_running());
    }

    #[tokio::test]
    async fn test_video_backlog_is_bounded_and_drops_excess_frames() {
        let session = HostSessionEngine::new(
            "bounded-video-backlog".to_string(),
            CaptureBackendType::WaylandPipeWire,
            EncoderConfig {
                width: 64,
                height: 64,
                target_fps: 200,
                ..EncoderConfig::default()
            },
        )
        .expect("HostSessionEngine::new should succeed");

        let (outgoing_tx, _outgoing_rx) = mpsc::channel::<WireMessage>(256);
        let (video_tx, mut video_rx) = mpsc::channel::<WireMessage>(MAX_OUTBOUND_VIDEO_BACKLOG);

        session
            .run_capture_loop_with_video(outgoing_tx, video_tx)
            .await
            .expect("run_capture_loop_with_video should succeed");

        // Wait until the bounded video channel fills without draining it.
        tokio::time::timeout(Duration::from_secs(2), async {
            while video_rx.len() < MAX_OUTBOUND_VIDEO_BACKLOG {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Timed out waiting for video backlog to fill");

        // Allow several more frame intervals to elapse while video_rx stays full.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(video_rx.len() <= MAX_OUTBOUND_VIDEO_BACKLOG);

        // Drain the buffered frames and verify the capture loop kept running and dropped excess frames.
        let mut drained = 0usize;
        while video_rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(drained, MAX_OUTBOUND_VIDEO_BACKLOG);

        let next = tokio::time::timeout(Duration::from_secs(2), video_rx.recv())
            .await
            .expect("Timed out waiting for video frame after draining backlog")
            .expect("Video channel closed unexpectedly");
        let WireMessage::Video(frame) = next else {
            panic!("Expected WireMessage::Video, got {next:?}");
        };
        assert!(
            frame.frame_id > MAX_OUTBOUND_VIDEO_BACKLOG as u64,
            "Expected dropped frames to advance frame_id beyond {MAX_OUTBOUND_VIDEO_BACKLOG}, got {}",
            frame.frame_id
        );

        // Stopping the session must terminate the capture loop and drop its video_tx cleanly.
        session.stop();
        tokio::time::timeout(Duration::from_secs(2), async {
            while video_rx.recv().await.is_some() {}
        })
        .await
        .expect("Timed out waiting for capture loop to close video channel after stop");
        assert!(!session.is_running());
    }
}
