//! Client Session Pipeline Demuxer, Sequence Gap `LossReport` & `FrameAck` Feedback Loop.
//!
//! Coordinates incoming wire packet demuxing (`HandshakeMessage`, `TopologyMessage`, `VideoFrameMessage`,
//! `AudioFrameMessage`, `ControlMessage`, `VirtualChannelMessage`), video sequence gap detection for
//! instant IDR keyframe recovery (`ControlMessage::LossReport`), microsecond `ControlMessage::FrameAck`
//! telemetry, and outbound peripheral input event forwarding.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{debug, error, info, warn};

use anyhow::{Context, Result, bail};
use perigee_protocol::{
    AudioCodec, ClipboardPayload, ControlMessage, ElementState, HandshakeMessage, InputEvent,
    KeyModifiers, MouseButtonCode, VideoCodec, VirtualChannelMessage, WireMessage,
};

use crate::audio::ClientAudioBuffer;
use crate::render::DecodedFrameSurface;

/// Upper bound on frame IDs listed in one `LossReport`; a larger gap still forces a keyframe.
pub const MAX_LOSS_REPORT_FRAMES: usize = 64;

/// Session configuration options supplied during viewer initialization.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub profile_id: String,
    pub host_address: String,
    pub vpn_adapter_binding: Option<String>,
    pub enable_audio: bool,
    pub enable_microphone: bool,
    pub enable_clipboard: bool,
    pub max_target_fps: u32,
    pub preferred_video_codec: VideoCodec,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            profile_id: String::from("default"),
            host_address: String::from("127.0.0.1:4433"),
            vpn_adapter_binding: None,
            enable_audio: true,
            enable_microphone: false,
            enable_clipboard: true,
            max_target_fps: 60,
            preferred_video_codec: VideoCodec::AV1,
        }
    }
}

/// Dynamic metrics captured during an active remote session.
#[derive(Debug, Clone, Default)]
pub struct SessionMetrics {
    pub current_fps: f32,
    pub rtt_ms: f32,
    pub decode_latency_us: u32,
    pub render_latency_us: u32,
    pub total_video_frames_received: u64,
    pub total_bytes_received: u64,
    pub packet_loss_rate: f32,
}

/// Represents the active state of a remote session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Authenticating,
    Active,
    /// Reserved for session resume (Phase 4); not sent yet.
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    Terminated {
        reason: String,
    },
}

/// Main Session Controller for managing a single active remote desktop connection.
///
/// Session lifecycle progresses through `Connecting` → `Authenticating` → `Active` → `Terminated`.
/// [`crate::bridge::QuicSessionBridge::disconnect`] (or [`crate::connect::ClientSession::disconnect`])
/// is the only teardown path.
pub struct ClientSessionPipeline {
    pub config: SessionConfig,
    pub state: watch::Sender<SessionState>,
    pub metrics: Arc<Mutex<SessionMetrics>>,
    pub audio_buffer: Arc<Mutex<ClientAudioBuffer>>,
    pub wire_tx: mpsc::Sender<WireMessage>,
    pub video_frame_tx: mpsc::Sender<DecodedFrameSurface>,
    pub input_event_tx: mpsc::Sender<InputEvent>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl ClientSessionPipeline {
    /// Spawns a new client session pipeline and hooks into transport & rendering channels.
    pub fn spawn(
        config: SessionConfig,
        wire_rx: mpsc::Receiver<WireMessage>,
        wire_tx: mpsc::Sender<WireMessage>,
    ) -> (
        Self,
        watch::Receiver<SessionState>,
        mpsc::Receiver<DecodedFrameSurface>,
        mpsc::Sender<InputEvent>,
    ) {
        let (state_tx, state_rx) = watch::channel(SessionState::Connecting);
        let (video_tx, video_rx) = mpsc::channel(128);
        let (input_tx, mut input_rx) = mpsc::channel(256);
        let metrics = Arc::new(Mutex::new(SessionMetrics::default()));
        let audio_buffer = Arc::new(Mutex::new(ClientAudioBuffer::default()));

        // Task 1: Demux incoming wire messages from transport
        let wire_tx_feedback = wire_tx.clone();
        let metrics_demux = metrics.clone();
        let audio_demux = audio_buffer.clone();
        let enable_audio = config.enable_audio;
        let state_demux = state_tx.clone();
        let video_tx_demux = video_tx.clone();
        let demux_handle = tokio::spawn(async move {
            Self::demux_loop(
                wire_rx,
                video_tx_demux,
                wire_tx_feedback,
                metrics_demux,
                audio_demux,
                enable_audio,
                state_demux,
            )
            .await;
        });

        // Task 2: Outbound input capture event queue
        let wire_tx_input = wire_tx.clone();
        let mut state_input_rx = state_rx.clone();
        let input_handle = tokio::spawn(async move {
            let wait_terminated = async {
                loop {
                    if matches!(
                        *state_input_rx.borrow_and_update(),
                        SessionState::Terminated { .. }
                    ) {
                        break;
                    }
                    if state_input_rx.changed().await.is_err() {
                        break;
                    }
                }
            };
            let forward_input = async {
                while let Some(input) = input_rx.recv().await {
                    let msg = WireMessage::Input(input);
                    if let Err(e) = wire_tx_input.send(msg).await {
                        error!("Failed to dispatch input event over QUIC transport: {e}");
                        break;
                    }
                }
            };
            tokio::select! {
                _ = wait_terminated => {}
                _ = forward_input => {}
            }
        });

        // Task 3: Initial Handshake sequence over QUIC
        let wire_tx_handshake = wire_tx.clone();
        let state_handshake = state_tx.clone();
        let config_handshake = config.clone();
        let hello_handle = tokio::spawn(async move {
            info!(
                "Initiating session handshake with target {}",
                config_handshake.host_address
            );
            let handshake = WireMessage::Handshake(HandshakeMessage::ClientHello {
                protocol_version: 1,
                client_id: String::from("perigee-client-v0.1"),
                client_name: String::from(get_platform_client_name()),
                supported_codecs: vec![
                    config_handshake.preferred_video_codec,
                    VideoCodec::H265,
                    VideoCodec::RawBgra,
                ],
            });
            if wire_tx_handshake.send(handshake).await.is_ok() {
                state_handshake.send_if_modified(|state| {
                    if *state == SessionState::Connecting {
                        *state = SessionState::Authenticating;
                        true
                    } else {
                        false
                    }
                });
            }
        });

        let pipeline = Self {
            config,
            state: state_tx,
            metrics,
            audio_buffer,
            wire_tx,
            video_frame_tx: video_tx,
            input_event_tx: input_tx.clone(),
            tasks: std::sync::Mutex::new(vec![demux_handle, input_handle, hello_handle]),
        };

        (pipeline, state_rx, video_rx, input_tx)
    }

    pub(crate) fn take_tasks(&self) -> Vec<tokio::task::JoinHandle<()>> {
        match self.tasks.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }

    pub(crate) fn tasks_finished(&self) -> bool {
        let guard = match self.tasks.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        !guard.is_empty() && guard.iter().all(tokio::task::JoinHandle::is_finished)
    }

    /// Asynchronous demuxing loop for incoming wire packets.
    async fn demux_loop(
        mut wire_rx: mpsc::Receiver<WireMessage>,
        video_tx: mpsc::Sender<DecodedFrameSurface>,
        wire_tx_feedback: mpsc::Sender<WireMessage>,
        metrics: Arc<Mutex<SessionMetrics>>,
        audio_buffer: Arc<Mutex<ClientAudioBuffer>>,
        enable_audio: bool,
        state_tx: watch::Sender<SessionState>,
    ) {
        let mut frame_count = 0u64;
        let mut expected_frame_id: Option<u64> = None;
        let mut last_fps_check = Instant::now();
        let mut state_rx = state_tx.subscribe();

        let wait_terminated = async {
            loop {
                if matches!(
                    *state_rx.borrow_and_update(),
                    SessionState::Terminated { .. }
                ) {
                    break;
                }
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        };

        let process_messages = async {
            while let Some(msg) = wire_rx.recv().await {
                match msg {
                    WireMessage::Handshake(h) => {
                        info!(?h, "Handshake update received");
                        match h {
                            HandshakeMessage::AuthResponse { success: true, .. } => {
                                let _ = state_tx.send(SessionState::Active);
                            }
                            HandshakeMessage::AuthResponse {
                                success: false,
                                error_message,
                                ..
                            } => {
                                let _ = state_tx.send(SessionState::Terminated {
                                    reason: error_message
                                        .unwrap_or_else(|| "authentication rejected".into()),
                                });
                                break;
                            }
                            _ => {}
                        }
                    }
                    WireMessage::Topology(t) => {
                        info!(?t, "Host display topology updated");
                    }
                    WireMessage::Video(frame) => {
                        let frame_idx = frame.frame_id;
                        let timestamp_us = frame.timestamp_us;
                        let payload_len = frame.payload.len() as u64;

                        // Detect sequence gaps and emit instant LossReport for IDR keyframe recovery
                        if let Some(expected) = expected_frame_id {
                            if frame_idx > expected {
                                let missing = (expected..frame_idx)
                                    .take(MAX_LOSS_REPORT_FRAMES)
                                    .collect::<Vec<u64>>();
                                warn!(
                                    gap = frame_idx - expected,
                                    reported = missing.len(),
                                    "Detected video frame sequence gap; sending LossReport"
                                );
                                let _ = wire_tx_feedback
                                    .send(WireMessage::Control(ControlMessage::LossReport {
                                        missing_frames: missing,
                                    }))
                                    .await;
                            } else if frame_idx < expected {
                                debug!(frame_idx, expected, "late or duplicate video frame");
                            }
                        }
                        expected_frame_id = Some(
                            expected_frame_id
                                .unwrap_or(0)
                                .max(frame_idx.saturating_add(1)),
                        );

                        let decode_start = Instant::now();
                        let decoded_surface = DecodedFrameSurface {
                            frame_index: frame_idx,
                            width: frame.width,
                            height: frame.height,
                            format: frame.format,
                            data: frame.payload,
                            timestamp_us,
                        };
                        let decode_dur = decode_start.elapsed().as_micros() as u32;

                        if video_tx.send(decoded_surface).await.is_ok() {
                            frame_count += 1;
                            let render_dur = decode_start.elapsed().as_micros() as u32;

                            {
                                let mut m = metrics.lock().await;
                                m.total_video_frames_received += 1;
                                m.total_bytes_received += payload_len;
                                m.decode_latency_us = decode_dur;
                                m.render_latency_us = render_dur;

                                if last_fps_check.elapsed() >= Duration::from_secs(1) {
                                    m.current_fps =
                                        frame_count as f32 / last_fps_check.elapsed().as_secs_f32();
                                    frame_count = 0;
                                    last_fps_check = Instant::now();
                                }
                            }

                            let ack = WireMessage::Control(ControlMessage::FrameAck {
                                frame_id: frame_idx,
                                monitor_id: frame.monitor_id,
                                render_delay_us: render_dur,
                            });
                            let _ = wire_tx_feedback.send(ack).await;
                        }
                    }
                    WireMessage::Audio(audio) => {
                        let payload_len = audio.payload.len() as u64;
                        debug!(
                            payload_bytes = payload_len,
                            codec = ?audio.codec,
                            "Low-latency audio frame received"
                        );
                        {
                            let mut m = metrics.lock().await;
                            m.total_bytes_received += payload_len;
                        }
                        if enable_audio {
                            let mut buf = audio_buffer.lock().await;
                            if let Err(e) = buf.push_frame(audio) {
                                warn!(error = %e, "Dropped incoming audio frame");
                            }
                        }
                    }
                    WireMessage::Control(ctrl) => match ctrl {
                        ControlMessage::BandwidthEstimate { rtt_ms, .. } => {
                            let mut m = metrics.lock().await;
                            m.rtt_ms = rtt_ms;
                        }
                        ControlMessage::LossReport { missing_frames } => {
                            warn!(
                                missing = missing_frames.len(),
                                first = ?missing_frames.first(),
                                last = ?missing_frames.last(),
                                "Loss report received from host"
                            );
                        }
                        _ => {}
                    },
                    WireMessage::Channel(VirtualChannelMessage::Clipboard(payload)) => {
                        let (kind, bytes) = match &payload {
                            ClipboardPayload::Text(text) => ("text", text.len()),
                            ClipboardPayload::Image { data, .. } => ("image", data.len()),
                            ClipboardPayload::Files(paths) => {
                                ("files", paths.iter().map(String::len).sum())
                            }
                        };
                        debug!(kind, bytes, "Virtual channel clipboard payload received");
                    }
                    WireMessage::Input(_) => {}
                }
            }
        };

        tokio::select! {
            _ = wait_terminated => {}
            _ = process_messages => {}
        }
    }

    /// Packages and dispatches a client microphone audio frame (`ClientToHostMicrophone`) over the session wire channel.
    pub async fn send_microphone_frame(
        &self,
        stream_id: u32,
        timestamp_us: u64,
        codec: AudioCodec,
        payload: Vec<u8>,
    ) -> Result<()> {
        if !self.config.enable_microphone {
            bail!("Microphone passthrough is disabled in SessionConfig");
        }
        let frame =
            ClientAudioBuffer::build_microphone_frame(stream_id, timestamp_us, codec, payload)?;
        self.wire_tx
            .send(WireMessage::Audio(frame))
            .await
            .context("Failed to send microphone audio frame over session wire channel")
    }

    /// Helper to inject mouse move events into the input channel
    pub async fn send_mouse_move(&self, monitor_id: u32, x: f64, y: f64, is_absolute: bool) {
        let _ = self
            .input_event_tx
            .send(InputEvent::MouseMove {
                monitor_id,
                x,
                y,
                is_absolute,
            })
            .await;
    }

    /// Helper to inject mouse button events
    pub async fn send_mouse_button(
        &self,
        button: MouseButtonCode,
        state: ElementState,
        x: f64,
        y: f64,
    ) {
        let _ = self
            .input_event_tx
            .send(InputEvent::MouseButton {
                button,
                state,
                x,
                y,
            })
            .await;
    }

    /// Helper to inject keypress events with platform modifier awareness
    pub async fn send_key_event(
        &self,
        key_code: u32,
        scancode: u32,
        state: ElementState,
        modifiers: KeyModifiers,
    ) {
        let _ = self
            .input_event_tx
            .send(InputEvent::KeyEvent {
                key_code,
                scancode,
                state,
                modifiers,
            })
            .await;
    }
}

/// Returns the platform-specific viewer client descriptor reported during `ClientHello`.
pub fn get_platform_client_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Perigee macOS Client (Metal)"
    } else if cfg!(target_os = "windows") {
        "Perigee Windows Client (DirectX 12)"
    } else {
        "Perigee Linux Client (Wayland/COSMIC)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perigee_protocol::{PixelFormat, VideoFrameMessage};

    #[tokio::test]
    async fn test_client_pipeline_handshake_and_loss_detection() {
        let (host_to_client_tx, host_to_client_rx) = mpsc::channel(32);
        let (client_to_host_tx, mut client_to_host_rx) = mpsc::channel(32);

        let (pipeline, _state_rx, mut video_rx, _input_tx) = ClientSessionPipeline::spawn(
            SessionConfig::default(),
            host_to_client_rx,
            client_to_host_tx,
        );

        // 1. Expect initial ClientHello from pipeline
        let hello = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            hello,
            WireMessage::Handshake(HandshakeMessage::ClientHello { .. })
        ));

        // 2. Send Frame 1 and Frame 3 (skipping Frame 2) to trigger LossReport
        host_to_client_tx
            .send(WireMessage::Video(VideoFrameMessage {
                frame_id: 1,
                monitor_id: 1,
                timestamp_us: 1000,
                codec: VideoCodec::AV1,
                is_keyframe: true,
                width: 1920,
                height: 1080,
                stride: 1920 * 4,
                format: PixelFormat::Bgra8,
                payload: vec![0xAA; 64],
            }))
            .await
            .unwrap();

        let decoded_1 = video_rx.recv().await.unwrap();
        assert_eq!(decoded_1.frame_index, 1);

        let ack_1 = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            ack_1,
            WireMessage::Control(ControlMessage::FrameAck { frame_id: 1, .. })
        ));

        host_to_client_tx
            .send(WireMessage::Video(VideoFrameMessage {
                frame_id: 3,
                monitor_id: 1,
                timestamp_us: 2000,
                codec: VideoCodec::AV1,
                is_keyframe: false,
                width: 1920,
                height: 1080,
                stride: 1920 * 4,
                format: PixelFormat::Bgra8,
                payload: vec![0xBB; 64],
            }))
            .await
            .unwrap();

        let loss_msg = client_to_host_rx.recv().await.unwrap();
        assert_eq!(
            loss_msg,
            WireMessage::Control(ControlMessage::LossReport {
                missing_frames: vec![2],
            })
        );

        let decoded_3 = video_rx.recv().await.unwrap();
        assert_eq!(decoded_3.frame_index, 3);

        let ack_3 = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            ack_3,
            WireMessage::Control(ControlMessage::FrameAck { frame_id: 3, .. })
        ));

        let m = pipeline.metrics.lock().await;
        assert_eq!(m.total_video_frames_received, 2);
        assert_eq!(m.total_bytes_received, 128);
    }

    #[tokio::test]
    async fn test_client_pipeline_auth_transition_bandwidth_and_input_forwarding() {
        let (host_to_client_tx, host_to_client_rx) = mpsc::channel(32);
        let (client_to_host_tx, mut client_to_host_rx) = mpsc::channel(32);

        let (pipeline, mut state_rx, _video_rx, _input_tx) = ClientSessionPipeline::spawn(
            SessionConfig::default(),
            host_to_client_rx,
            client_to_host_tx,
        );

        // Drain initial ClientHello
        let _hello = client_to_host_rx.recv().await.unwrap();
        while *state_rx.borrow_and_update() != SessionState::Authenticating {
            state_rx.changed().await.unwrap();
        }
        assert_eq!(*state_rx.borrow(), SessionState::Authenticating);

        // Verify AuthResponse transitions session state to Active
        host_to_client_tx
            .send(WireMessage::Handshake(HandshakeMessage::AuthResponse {
                success: true,
                session_token: Some(String::from("tok-123")),
                requires_elevation: false,
                error_message: None,
            }))
            .await
            .unwrap();

        while *state_rx.borrow_and_update() != SessionState::Active {
            state_rx.changed().await.unwrap();
        }
        assert_eq!(*state_rx.borrow(), SessionState::Active);

        // Verify BandwidthEstimate updates SessionMetrics::rtt_ms
        host_to_client_tx
            .send(WireMessage::Control(ControlMessage::BandwidthEstimate {
                bytes_per_sec: 25_000_000,
                rtt_ms: 4.25,
                jitter_ms: 0.5,
            }))
            .await
            .unwrap();

        // Dispatch input events and verify wire forwarding
        pipeline.send_mouse_move(1, 0.5, 0.25, true).await;
        let move_msg = client_to_host_rx.recv().await.unwrap();
        assert_eq!(
            move_msg,
            WireMessage::Input(InputEvent::MouseMove {
                monitor_id: 1,
                x: 0.5,
                y: 0.25,
                is_absolute: true,
            })
        );

        pipeline
            .send_mouse_button(MouseButtonCode::Left, ElementState::Pressed, 0.5, 0.25)
            .await;
        let button_msg = client_to_host_rx.recv().await.unwrap();
        assert_eq!(
            button_msg,
            WireMessage::Input(InputEvent::MouseButton {
                button: MouseButtonCode::Left,
                state: ElementState::Pressed,
                x: 0.5,
                y: 0.25,
            })
        );

        let mods = KeyModifiers {
            shift: true,
            ctrl: false,
            alt: false,
            logo: false,
        };
        pipeline
            .send_key_event(65, 30, ElementState::Pressed, mods)
            .await;
        let key_msg = client_to_host_rx.recv().await.unwrap();
        assert_eq!(
            key_msg,
            WireMessage::Input(InputEvent::KeyEvent {
                key_code: 65,
                scancode: 30,
                state: ElementState::Pressed,
                modifiers: mods,
            })
        );

        let m = pipeline.metrics.lock().await;
        assert!((m.rtt_ms - 4.25).abs() < f32::EPSILON);
    }

    fn make_test_video_frame(frame_id: u64) -> WireMessage {
        WireMessage::Video(VideoFrameMessage {
            frame_id,
            monitor_id: 1,
            timestamp_us: 1000,
            codec: VideoCodec::AV1,
            is_keyframe: frame_id == 1,
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            format: PixelFormat::Bgra8,
            payload: vec![0xAA; 64],
        })
    }

    #[tokio::test]
    async fn test_loss_report_is_bounded() {
        let (host_to_client_tx, host_to_client_rx) = mpsc::channel(32);
        let (client_to_host_tx, mut client_to_host_rx) = mpsc::channel(32);

        let (_pipeline, _state_rx, mut video_rx, _input_tx) = ClientSessionPipeline::spawn(
            SessionConfig::default(),
            host_to_client_rx,
            client_to_host_tx,
        );

        let hello = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            hello,
            WireMessage::Handshake(HandshakeMessage::ClientHello { .. })
        ));

        host_to_client_tx
            .send(make_test_video_frame(1))
            .await
            .unwrap();
        let _ = video_rx.recv().await.unwrap();
        let ack_1 = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            ack_1,
            WireMessage::Control(ControlMessage::FrameAck { frame_id: 1, .. })
        ));

        host_to_client_tx
            .send(make_test_video_frame(1_000_000))
            .await
            .unwrap();
        let _ = video_rx.recv().await.unwrap();

        let loss_msg = client_to_host_rx.recv().await.unwrap();
        match loss_msg {
            WireMessage::Control(ControlMessage::LossReport { missing_frames }) => {
                assert_eq!(missing_frames.len(), MAX_LOSS_REPORT_FRAMES);
                assert_eq!(missing_frames[0], 2);
            }
            other => panic!("expected LossReport, got {other:?}"),
        }

        let ack_big = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            ack_big,
            WireMessage::Control(ControlMessage::FrameAck {
                frame_id: 1_000_000,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_out_of_order_frame_does_not_rewind() {
        let (host_to_client_tx, host_to_client_rx) = mpsc::channel(32);
        let (client_to_host_tx, mut client_to_host_rx) = mpsc::channel(32);

        let (_pipeline, _state_rx, mut video_rx, _input_tx) = ClientSessionPipeline::spawn(
            SessionConfig::default(),
            host_to_client_rx,
            client_to_host_tx,
        );

        let hello = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            hello,
            WireMessage::Handshake(HandshakeMessage::ClientHello { .. })
        ));

        for frame_id in [1, 3, 2, 4] {
            host_to_client_tx
                .send(make_test_video_frame(frame_id))
                .await
                .unwrap();
            let decoded = video_rx.recv().await.unwrap();
            assert_eq!(decoded.frame_index, frame_id);
        }

        let mut loss_reports = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(msg) = client_to_host_rx.recv().await {
                if let WireMessage::Control(ControlMessage::LossReport { missing_frames }) = msg {
                    loss_reports.push(missing_frames);
                }
            }
        })
        .await;

        assert_eq!(loss_reports, vec![vec![2]]);
    }

    #[tokio::test]
    async fn test_max_frame_id_does_not_overflow() {
        let (host_to_client_tx, host_to_client_rx) = mpsc::channel(32);
        let (client_to_host_tx, mut client_to_host_rx) = mpsc::channel(32);

        let (_pipeline, _state_rx, mut video_rx, _input_tx) = ClientSessionPipeline::spawn(
            SessionConfig::default(),
            host_to_client_rx,
            client_to_host_tx,
        );

        let hello = client_to_host_rx.recv().await.unwrap();
        assert!(matches!(
            hello,
            WireMessage::Handshake(HandshakeMessage::ClientHello { .. })
        ));

        for frame_id in [u64::MAX, 5] {
            host_to_client_tx
                .send(make_test_video_frame(frame_id))
                .await
                .unwrap();
            let decoded = video_rx.recv().await.unwrap();
            assert_eq!(decoded.frame_index, frame_id);
        }

        for expected_ack_id in [u64::MAX, 5] {
            let msg = client_to_host_rx.recv().await.unwrap();
            assert_eq!(
                msg,
                WireMessage::Control(ControlMessage::FrameAck {
                    frame_id: expected_ack_id,
                    monitor_id: 1,
                    render_delay_us: match msg {
                        WireMessage::Control(ControlMessage::FrameAck {
                            render_delay_us, ..
                        }) => render_delay_us,
                        _ => 0,
                    },
                })
            );
        }

        assert!(client_to_host_rx.try_recv().is_err());
    }
}
