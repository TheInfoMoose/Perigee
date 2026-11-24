//! `perigee-host`: Remote Host Capture, Encoding, Input & Daemon Crate.
//!
//! Status: stand-in. Frame capture, video encoding, audio capture, and Linux input injection are stand-ins that emit synthetic buffers or log events; Phase 5 replaces them with real media and input backends.
//!
//! Coordinates host session management (`daemon`, `host`), platform capture and input traits
//! (`backends`), video frame packaging (`encoder`), and periodic audio frame emission (`audio`).

use anyhow::Result;

use perigee_protocol::{InputEvent, PixelFormat};

pub mod audio;
pub use audio::{
    HostAudioCapturer, OPUS_CHANNELS, OPUS_FRAME_INTERVAL_MS, OPUS_PAYLOAD_BYTES,
    OPUS_SAMPLE_RATE_HZ,
};

pub mod backends;
#[cfg(target_os = "linux")]
pub use backends::WaylandShmCapturer;
#[cfg(target_os = "linux")]
pub use backends::wayland_shm::{format_monitor_list, list_wayland_outputs};
pub use backends::{
    CG_EVENT_FLAG_MASK_ALTERNATE, CG_EVENT_FLAG_MASK_COMMAND, CG_EVENT_FLAG_MASK_CONTROL,
    CG_EVENT_FLAG_MASK_SHIFT, CV_PIXEL_FORMAT_32BGRA, CV_PIXEL_FORMAT_32RGBA, CV_PIXEL_FORMAT_420F,
    CV_PIXEL_FORMAT_420V, CV_PIXEL_FORMAT_Y420, CV_PIXEL_FORMAT_Y444, CgMouseEventKind,
    DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_NV12, DRM_FORMAT_XBGR8888,
    DRM_FORMAT_XRGB8888, DXGI_FORMAT_B8G8R8A8_UNORM_RAW, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB_RAW,
    DXGI_FORMAT_NV12_RAW, DXGI_FORMAT_R8G8B8A8_UNORM_RAW, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB_RAW,
    DmaBufPlaneDescriptor, DxgiSubresourceLayout, EVDEV_BTN_EXTRA, EVDEV_BTN_LEFT,
    EVDEV_BTN_MIDDLE, EVDEV_BTN_RIGHT, EVDEV_BTN_SIDE, FrameCapturer, GbmDmabufSurface,
    IoSurfacePlaneDescriptor, LinuxCaptureController, LinuxInputController, MacOsCgEventInjector,
    MacOsIoSurfaceDescriptor, MacOsScreenCaptureKitCapturer, NormalizedPointerCoords,
    TranslatedCgEvent, TranslatedWin32Input, VK_CONTROL_CODE, VK_LWIN_CODE, VK_MENU_CODE,
    VK_SHIFT_CODE, WIN32_KEYEVENTF_EXTENDEDKEY, WIN32_KEYEVENTF_KEYUP, WIN32_KEYEVENTF_SCANCODE,
    WIN32_KEYEVENTF_UNICODE, WIN32_MOUSEEVENTF_ABSOLUTE, WIN32_MOUSEEVENTF_HWHEEL,
    WIN32_MOUSEEVENTF_LEFTDOWN, WIN32_MOUSEEVENTF_LEFTUP, WIN32_MOUSEEVENTF_MIDDLEDOWN,
    WIN32_MOUSEEVENTF_MIDDLEUP, WIN32_MOUSEEVENTF_MOVE, WIN32_MOUSEEVENTF_RIGHTDOWN,
    WIN32_MOUSEEVENTF_RIGHTUP, WIN32_MOUSEEVENTF_VIRTUALDESK, WIN32_MOUSEEVENTF_WHEEL,
    WIN32_MOUSEEVENTF_XDOWN, WIN32_MOUSEEVENTF_XUP, WIN32_WHEEL_DELTA, WIN32_XBUTTON1,
    WIN32_XBUTTON2, WaylandDmabufCapturer, WaylandVirtualInputInjector, WindowsDxgiCapturer,
    WindowsDxgiSurfaceDescriptor, WindowsSendInputInjector, X11_BUTTON_BACK, X11_BUTTON_FORWARD,
    X11_BUTTON_LEFT, X11_BUTTON_MIDDLE, X11_BUTTON_RIGHT, X11_BUTTON_SCROLL_DOWN,
    X11_BUTTON_SCROLL_LEFT, X11_BUTTON_SCROLL_RIGHT, X11_BUTTON_SCROLL_UP, X11ShmCapturer,
    X11XTestInputInjector, XKB_EVDEV_OFFSET, encode_win32_modifier_transitions,
    encode_xkb_modifiers_mask, evdev_to_x11_keycode, map_cv_pixel_format_to_wire,
    map_drm_fourcc_to_pixel_format, map_dxgi_format_to_wire, map_scancode_to_macos_keycode,
    map_scancode_to_win32_vk, mouse_button_to_evdev, mouse_button_to_x11_detail,
    normalize_pointer_coordinates, normalize_win32_absolute_coords,
    normalize_win32_virtual_desktop_coords, translate_input_event_to_cg,
    translate_input_event_to_win32, translate_key_to_evdev_scancode,
    translate_modifiers_to_cg_flags,
};

pub mod daemon;
pub use daemon::{CLIENT_HELLO_TIMEOUT, HostSessionEngine, PerigeeHostDaemon};

pub mod encoder;
pub use encoder::{
    EncoderConfig, MacOsVideoToolboxEncoder, RAW_FRAME_MAX_BYTES, RawVideoEncoder,
    SoftwareVideoEncoder, VideoEncoder, fit_raw_frame,
};

pub mod encoder_gst;
pub use encoder_gst::GstreamerAv1Encoder;

pub mod ppm;
pub use ppm::encode_ppm;

#[cfg(target_os = "linux")]
pub mod seat;
#[cfg(target_os = "linux")]
pub use seat::{
    ActiveSession, LoginctlRunner, SystemLoginctl, find_wayland_socket, query_active_session,
};

pub mod host;
pub use host::{
    DEFAULT_BIND_ADDR, DEFAULT_NODE_NAME, HOST_USAGE, HostAdmin, HostCommand, HostOptions,
    HostTool, PairArgs, TrustFileAuthorizer, bind_host, host_config_dir, parse_host_args,
    resolve_host_command, run_host, run_host_admin, run_host_pair, run_host_tool, shutdown_signal,
};

/// Operating System Display & Frame Capture Backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackendType {
    /// Linux Wayland capture backend selector
    WaylandPipeWire,
    /// Windows DXGI Desktop Duplication backend selector
    WindowsDxgi,
    /// macOS ScreenCaptureKit backend selector
    MacOsScreenCaptureKit,
    /// Linux X11 SHM backend selector
    X11Fallback,
}

/// Which media backends a host session uses (`perigee-host --media`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaMode {
    /// Stand-in capture, encoder and input (synthetic frames, logged input). The default, so
    /// tests and dev runs never touch a real screen or pointer.
    #[default]
    StandIn,
    /// Linux only: capture the compositor this process reaches through `WAYLAND_DISPLAY`
    /// (`ext-image-copy-capture-v1`, output 0), inject input through `/dev/uinput`, and send
    /// raw frames at [`daemon::LIVE_RAW_FPS`].
    Live,
    /// Linux only: follow logind's active session on seat0 (greeter → user → greeter) through a
    /// `capture-worker` running as that session's user; input through `/dev/uinput`. For the
    /// packaged service.
    FollowSeat,
}

/// Video codec a host session uses (`perigee-host --codec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostCodec {
    /// Uncompressed raw frames (default).
    #[default]
    Raw,
    /// GStreamer AV1 encoder (`vaav1enc` hardware VA-API preferred, `av1enc` software fallback).
    Av1,
}

/// Raw Frame Buffer captured from Host GPU / Display Subsystem
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub frame_index: u64,
    pub timestamp_us: u64,
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: PixelFormat,
    pub data: Vec<u8>,
    pub is_dmabuf_or_metal_handle: bool,
}

/// Abstract Native Host Input Injector (Wayland Smithay, Windows SendInput & macOS CGEvent)
pub trait HostInputInjector: Send + Sync {
    fn inject_input(&self, event: &InputEvent) -> Result<()>;
}

/// Cross-Platform Native Input Injector Implementation
pub struct NativeInputInjector {
    backend: CaptureBackendType,
    linux_controller: Option<LinuxInputController>,
    macos_injector: Option<MacOsCgEventInjector>,
    windows_injector: Option<WindowsSendInputInjector>,
}

impl NativeInputInjector {
    pub fn new(backend: CaptureBackendType) -> Self {
        let linux_controller = match backend {
            CaptureBackendType::WaylandPipeWire | CaptureBackendType::X11Fallback => {
                LinuxInputController::new(backend, 2560, 1440).ok()
            }
            _ => None,
        };
        let macos_injector = match backend {
            CaptureBackendType::MacOsScreenCaptureKit => {
                Some(MacOsCgEventInjector::new_live(2560, 1440))
            }
            _ => None,
        };
        let windows_injector = match backend {
            CaptureBackendType::WindowsDxgi => Some(WindowsSendInputInjector::new_live(2560, 1440)),
            _ => None,
        };
        Self {
            backend,
            linux_controller,
            macos_injector,
            windows_injector,
        }
    }
}

impl HostInputInjector for NativeInputInjector {
    fn inject_input(&self, event: &InputEvent) -> Result<()> {
        match self.backend {
            CaptureBackendType::MacOsScreenCaptureKit => {
                if let Some(injector) = &self.macos_injector {
                    injector.inject_input(event)?;
                }
            }
            CaptureBackendType::WaylandPipeWire | CaptureBackendType::X11Fallback => {
                if let Some(ctrl) = &self.linux_controller {
                    ctrl.inject_input(event)?;
                }
            }
            CaptureBackendType::WindowsDxgi => {
                if let Some(injector) = &self.windows_injector {
                    injector.inject_input(event)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tokio::sync::mpsc;

    use perigee_protocol::{
        AudioCodec, AudioDirection, ControlMessage, HandshakeMessage, VideoCodec, WireMessage,
    };

    #[tokio::test]
    async fn test_forced_keyframe_on_packet_loss() {
        let mut encoder = SoftwareVideoEncoder::new(EncoderConfig {
            keyframe_interval: 5,
            ..Default::default()
        });
        let dummy_frame = CapturedFrame {
            frame_index: 1,
            timestamp_us: 100,
            display_id: 1,
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            pixel_format: PixelFormat::Bgra8,
            data: vec![0u8; 2048],
            is_dmabuf_or_metal_handle: true,
        };

        // Frame 1 is an initial keyframe
        let msg1 = encoder.encode(&dummy_frame, false).unwrap();
        assert!(msg1.is_keyframe);

        // Frame 2 is a normal inter-frame
        let msg2 = encoder.encode(&dummy_frame, false).unwrap();
        assert!(!msg2.is_keyframe);

        // Frame 3 with force_keyframe = true (packet loss recovery)
        let msg3 = encoder.encode(&dummy_frame, true).unwrap();
        assert!(msg3.is_keyframe);

        // Frame 4 returns to normal inter-frame
        let msg4 = encoder.encode(&dummy_frame, false).unwrap();
        assert!(!msg4.is_keyframe);

        // Frame 5 hits keyframe_interval multiple (5)
        let msg5 = encoder.encode(&dummy_frame, false).unwrap();
        assert!(msg5.is_keyframe);

        // Verify ControlMessage::LossReport triggers force_keyframe_flag on HostSessionEngine
        let session = HostSessionEngine::new(
            "test-session".to_string(),
            CaptureBackendType::WaylandPipeWire,
            EncoderConfig::default(),
        )
        .expect("HostSessionEngine::new should succeed");
        session.force_keyframe_flag.store(false, Ordering::SeqCst);
        session
            .handle_incoming_wire_message(WireMessage::Control(ControlMessage::LossReport {
                missing_frames: vec![2, 3],
            }))
            .await
            .unwrap();
        assert!(session.force_keyframe_flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_handle_handshake_accepts_only_client_hello() {
        let daemon = PerigeeHostDaemon::new(
            "test-host-daemon".to_string(),
            CaptureBackendType::WaylandPipeWire,
        );
        let hello = |protocol_version: u32, supported_codecs: Vec<VideoCodec>| {
            HandshakeMessage::ClientHello {
                protocol_version,
                client_id: "viewer-client-01".to_string(),
                client_name: "COSMIC Viewer".to_string(),
                supported_codecs,
            }
        };

        let session_id = daemon
            .handle_handshake(hello(1, vec![VideoCodec::AV1, VideoCodec::H265]))
            .await
            .expect("a valid ClientHello is accepted");
        assert!(!session_id.is_empty());

        assert!(
            daemon
                .handle_handshake(hello(0, vec![VideoCodec::AV1]))
                .await
                .is_err()
        );
        assert!(daemon.handle_handshake(hello(1, vec![])).await.is_err());

        let not_hello = HandshakeMessage::AuthResponse {
            success: true,
            session_token: None,
            error_message: None,
        };
        assert!(daemon.handle_handshake(not_hello).await.is_err());
    }

    #[tokio::test]
    async fn test_daemon_session_lifecycle_and_audio_loopback() {
        let daemon = PerigeeHostDaemon::new(
            "lifecycle-host".to_string(),
            CaptureBackendType::WaylandPipeWire,
        );
        assert_eq!(daemon.active_session_count().await, 0);

        let hello = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "client-lifecycle-1".to_string(),
            client_name: "Linux Viewer".to_string(),
            supported_codecs: vec![VideoCodec::AV1],
        };
        let session_id_1 = daemon.handle_handshake(hello).await.unwrap();
        let session_1 = daemon
            .provision_session(session_id_1.clone(), EncoderConfig::default())
            .await
            .expect("provision_session should succeed");
        let session_2 = daemon
            .provision_session("session-2".to_string(), EncoderConfig::default())
            .await
            .expect("provision_session should succeed");

        assert_eq!(daemon.active_session_count().await, 2);
        assert!(session_1.is_running());
        assert!(session_2.is_running());
        assert!(daemon.get_session(&session_id_1).await.is_some());

        // Start capture + 20ms Opus audio loopback on session_1 and verify emitted frames
        let (wire_tx, mut wire_rx) = mpsc::channel(32);
        session_1.run_capture_loop(wire_tx).await.unwrap();
        assert!(session_1.audio_capturer.is_capturing());

        let mut saw_video = false;
        let mut saw_audio = false;
        for _ in 0..10 {
            if let Some(msg) = wire_rx.recv().await {
                match msg {
                    WireMessage::Video(v) => {
                        assert_eq!(v.width, 2560);
                        assert_eq!(v.height, 1440);
                        saw_video = true;
                    }
                    WireMessage::Audio(a) => {
                        assert_eq!(a.codec, AudioCodec::Opus);
                        assert_eq!(a.direction, AudioDirection::HostToClientSpeaker);
                        assert_eq!(a.channels, OPUS_CHANNELS);
                        assert_eq!(a.sample_rate, OPUS_SAMPLE_RATE_HZ);
                        assert_eq!(a.payload.len(), OPUS_PAYLOAD_BYTES);
                        saw_audio = true;
                    }
                    _ => {}
                }
                if saw_video && saw_audio {
                    break;
                }
            }
        }
        assert!(saw_video, "Capture loop should emit VideoFrameMessage");
        assert!(
            saw_audio,
            "Audio loopback should emit 20ms Opus AudioFrameMessage"
        );

        // Terminate session_1 and verify it stops and is removed from active_sessions
        assert!(daemon.terminate_session(&session_id_1).await);
        assert!(!session_1.is_running());
        assert!(!session_1.audio_capturer.is_capturing());
        assert_eq!(daemon.active_session_count().await, 1);
        assert!(!daemon.terminate_session("non-existent-session").await);

        // Shutdown all remaining sessions
        daemon.shutdown_all_sessions().await;
        assert!(!session_2.is_running());
        assert_eq!(daemon.active_session_count().await, 0);
    }
}
