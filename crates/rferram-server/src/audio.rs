//! Host audio frame emission stand-in.
//!
//! Status: stand-in. `HostAudioCapturer` emits zero-filled 192-byte buffers every 20 ms with no OS audio loopback capture or codec; Phase 5 replaces or defers it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use rferram_protocol::{AudioCodec, AudioDirection, AudioFrameMessage, WireMessage};

use crate::CaptureBackendType;

/// Audio frame interval in milliseconds (50 Hz).
pub const OPUS_FRAME_INTERVAL_MS: u64 = 20;

/// Audio sample rate constant (48 kHz).
pub const OPUS_SAMPLE_RATE_HZ: u32 = 48_000;

/// Standard stereo channel count.
pub const OPUS_CHANNELS: u8 = 2;

/// Stand-in 20 ms audio payload size in bytes.
pub const OPUS_PAYLOAD_BYTES: usize = 192;

/// Stand-in host audio emitter that sends zero-filled frames on a 20 ms timer.
pub struct HostAudioCapturer {
    pub backend: CaptureBackendType,
    pub is_running: Arc<AtomicBool>,
}

impl HostAudioCapturer {
    /// Creates a new [`HostAudioCapturer`] recording the selected [`CaptureBackendType`].
    pub fn new(backend: CaptureBackendType) -> Self {
        Self {
            backend,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns `true` if the periodic audio frame emission task is currently running.
    pub fn is_capturing(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    /// Constructs a zero-filled 192-byte [`AudioFrameMessage`] tagged with [`AudioCodec::Opus`].
    pub fn build_audio_frame(stream_id: u32, timestamp_us: u64) -> AudioFrameMessage {
        AudioFrameMessage {
            stream_id,
            timestamp_us,
            direction: AudioDirection::HostToClientSpeaker,
            codec: AudioCodec::Opus,
            channels: OPUS_CHANNELS,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            payload: vec![0u8; OPUS_PAYLOAD_BYTES],
        }
    }

    /// Starts the asynchronous 50 Hz (20 ms) stand-in audio emission loop.
    pub async fn start_loopback_capture(&self, tx: mpsc::Sender<WireMessage>) -> Result<()> {
        self.is_running.store(true, Ordering::SeqCst);
        let is_running = self.is_running.clone();
        let backend = self.backend;

        tokio::spawn(async move {
            let mut stream_id: u32 = 1;
            let interval = Duration::from_millis(OPUS_FRAME_INTERVAL_MS);
            let start_instant = Instant::now();

            info!(?backend, "Started host audio loopback capture task");

            while is_running.load(Ordering::SeqCst) {
                tokio::time::sleep(interval).await;
                if !is_running.load(Ordering::SeqCst) {
                    break;
                }

                let audio_msg =
                    Self::build_audio_frame(stream_id, start_instant.elapsed().as_micros() as u64);

                if tx.send(WireMessage::Audio(audio_msg)).await.is_err() {
                    is_running.store(false, Ordering::SeqCst);
                    break;
                }
                stream_id += 1;
            }
        });

        Ok(())
    }

    /// Stops the active audio loopback capture loop.
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}
