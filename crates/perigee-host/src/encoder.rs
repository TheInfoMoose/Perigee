//! Video encoder trait and stand-in implementations.
//!
//! Status: stand-in. `SoftwareVideoEncoder` and `MacOsVideoToolboxEncoder` copy the first 1–2 KiB of the frame; no codec runs, and Phase 5 replaces them.

use anyhow::{Result, bail};
use tracing::{debug, info};

use perigee_protocol::{PixelFormat, VideoCodec, VideoFrameMessage};

use crate::CapturedFrame;
pub use crate::encoder_gst::GstreamerAv1Encoder;

/// Video Encoder Configuration
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
    pub target_bitrate_kbps: u32,
    pub enable_hardware_accel: bool,
    pub keyframe_interval: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::AV1,
            width: 2560,
            height: 1440,
            target_fps: 60,
            target_bitrate_kbps: 15_000,
            enable_hardware_accel: true,
            keyframe_interval: 60,
        }
    }
}

/// Trait for video frame encoders (currently implemented by stand-ins that slice raw frame bytes).
pub trait VideoEncoder: Send + Sync {
    fn encode(&mut self, frame: &CapturedFrame, force_keyframe: bool) -> Result<VideoFrameMessage>;
    fn update_bitrate(&mut self, target_bitrate_kbps: u32) -> Result<()>;
    fn update_framerate(&mut self, target_fps: u32) -> Result<()>;
}

/// Largest payload [`RawVideoEncoder`] sends per frame (4 MiB, a quarter of `MAX_WIRE_FRAME_BYTES`).
pub const RAW_FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Packs a 4-byte-per-pixel frame into `width * 4`-byte rows and, when that exceeds
/// `max_bytes`, keeps every n-th pixel of every n-th row for the smallest whole `n` that fits.
pub fn fit_raw_frame(frame: &CapturedFrame, max_bytes: usize) -> Result<CapturedFrame> {
    if !matches!(frame.pixel_format, PixelFormat::Bgra8 | PixelFormat::Rgba8) {
        bail!(
            "raw frames must be Bgra8 or Rgba8, not {:?}",
            frame.pixel_format
        );
    }
    if max_bytes < 4 {
        bail!("a {max_bytes}-byte budget cannot hold one pixel");
    }
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    if width == 0
        || height == 0
        || stride < width * 4
        || frame.data.len() < stride * (height - 1) + width * 4
    {
        bail!(
            "raw frame {}x{} at stride {} does not match its {} data bytes",
            frame.width,
            frame.height,
            frame.stride,
            frame.data.len()
        );
    }
    let mut step = 1usize;
    while (width / step).max(1) * (height / step).max(1) * 4 > max_bytes {
        step += 1;
    }
    let out_width = (width / step).max(1);
    let out_height = (height / step).max(1);
    let mut data = Vec::with_capacity(out_width * out_height * 4);
    for row in 0..out_height {
        let line = &frame.data[row * step * stride..];
        for col in 0..out_width {
            let at = col * step * 4;
            data.extend_from_slice(&line[at..at + 4]);
        }
    }
    Ok(CapturedFrame {
        frame_index: frame.frame_index,
        timestamp_us: frame.timestamp_us,
        display_id: frame.display_id,
        width: out_width as u32,
        height: out_height as u32,
        stride: (out_width * 4) as u32,
        pixel_format: frame.pixel_format,
        data,
        is_dmabuf_or_metal_handle: false,
    })
}

/// Sends whole frames uncompressed as `VideoCodec::RawBgra` (packed rows, fitted under
/// [`RAW_FRAME_MAX_BYTES`]; `format` says Bgra8 or Rgba8). Stand-in for a real codec until
/// Phase 5b: about 40 MB/s at 10 fps, so loopback or LAN only.
pub struct RawVideoEncoder {
    config: EncoderConfig,
    frame_counter: u64,
}

impl RawVideoEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        Self {
            config,
            frame_counter: 0,
        }
    }

    /// Returns a reference to the current encoder configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }
}

impl VideoEncoder for RawVideoEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame,
        _force_keyframe: bool,
    ) -> Result<VideoFrameMessage> {
        let fitted = fit_raw_frame(frame, RAW_FRAME_MAX_BYTES)?;
        self.frame_counter += 1;
        Ok(VideoFrameMessage {
            frame_id: self.frame_counter,
            monitor_id: fitted.display_id,
            timestamp_us: fitted.timestamp_us,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: fitted.width,
            height: fitted.height,
            stride: fitted.stride,
            format: fitted.pixel_format,
            payload: fitted.data,
        })
    }

    fn update_bitrate(&mut self, target_bitrate_kbps: u32) -> Result<()> {
        self.config.target_bitrate_kbps = target_bitrate_kbps;
        Ok(())
    }

    fn update_framerate(&mut self, target_fps: u32) -> Result<()> {
        self.config.target_fps = target_fps;
        Ok(())
    }
}

/// Stand-in macOS video encoder (slices up to 2048 bytes of `frame.data` without compression).
pub struct MacOsVideoToolboxEncoder {
    config: EncoderConfig,
    frame_counter: u64,
}

impl MacOsVideoToolboxEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        info!(
            width = config.width,
            height = config.height,
            codec = ?config.codec,
            "Initialized macOS VideoToolbox Hardware Encoder (Apple Silicon / Intel Media Engine)"
        );
        Self {
            config,
            frame_counter: 0,
        }
    }

    /// Returns a reference to the current encoder configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// Returns the total number of frames encoded so far.
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }
}

impl VideoEncoder for MacOsVideoToolboxEncoder {
    fn encode(&mut self, frame: &CapturedFrame, force_keyframe: bool) -> Result<VideoFrameMessage> {
        self.frame_counter += 1;
        let is_keyframe = force_keyframe
            || self.frame_counter == 1
            || self
                .frame_counter
                .is_multiple_of(self.config.keyframe_interval.max(1) as u64);

        let payload_size = std::cmp::min(2048, frame.data.len());
        let encoded_payload = frame.data[..payload_size].to_vec();

        Ok(VideoFrameMessage {
            frame_id: self.frame_counter,
            monitor_id: frame.display_id,
            timestamp_us: frame.timestamp_us,
            codec: self.config.codec,
            is_keyframe,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            format: frame.pixel_format,
            payload: encoded_payload,
        })
    }

    fn update_bitrate(&mut self, target_bitrate_kbps: u32) -> Result<()> {
        self.config.target_bitrate_kbps = target_bitrate_kbps;
        debug!(
            target_bitrate_kbps,
            "VideoToolbox target bitrate updated dynamically"
        );
        Ok(())
    }

    fn update_framerate(&mut self, target_fps: u32) -> Result<()> {
        self.config.target_fps = target_fps;
        debug!(
            target_fps,
            "VideoToolbox target framerate updated dynamically"
        );
        Ok(())
    }
}

/// Stand-in software video encoder (slices up to 1024 bytes of `frame.data` without compression).
pub struct SoftwareVideoEncoder {
    config: EncoderConfig,
    frame_counter: u64,
}

impl SoftwareVideoEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        Self {
            config,
            frame_counter: 0,
        }
    }

    /// Returns a reference to the current encoder configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// Returns the total number of frames encoded so far.
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }
}

impl VideoEncoder for SoftwareVideoEncoder {
    fn encode(&mut self, frame: &CapturedFrame, force_keyframe: bool) -> Result<VideoFrameMessage> {
        self.frame_counter += 1;
        let is_keyframe = force_keyframe
            || self.frame_counter == 1
            || self
                .frame_counter
                .is_multiple_of(self.config.keyframe_interval.max(1) as u64);

        let payload_size = std::cmp::min(1024, frame.data.len());
        let encoded_payload = frame.data[..payload_size].to_vec();

        Ok(VideoFrameMessage {
            frame_id: self.frame_counter,
            monitor_id: frame.display_id,
            timestamp_us: frame.timestamp_us,
            codec: self.config.codec,
            is_keyframe,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            format: frame.pixel_format,
            payload: encoded_payload,
        })
    }

    fn update_bitrate(&mut self, target_bitrate_kbps: u32) -> Result<()> {
        self.config.target_bitrate_kbps = target_bitrate_kbps;
        Ok(())
    }

    fn update_framerate(&mut self, target_fps: u32) -> Result<()> {
        self.config.target_fps = target_fps;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> CapturedFrame {
        CapturedFrame {
            frame_index: 1,
            timestamp_us: 100,
            display_id: 1,
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            pixel_format: PixelFormat::Bgra8,
            data: vec![0xAA; 2048],
            is_dmabuf_or_metal_handle: true,
        }
    }

    #[test]
    fn test_encoder_keyframe_interval_multiples_and_updates() {
        let mut sw_encoder = SoftwareVideoEncoder::new(EncoderConfig {
            keyframe_interval: 3,
            ..Default::default()
        });
        let frame = sample_frame();

        // Frame 1: initial keyframe
        let f1 = sw_encoder.encode(&frame, false).unwrap();
        assert_eq!(f1.frame_id, 1);
        assert!(f1.is_keyframe);

        // Frame 2: inter-frame
        let f2 = sw_encoder.encode(&frame, false).unwrap();
        assert_eq!(f2.frame_id, 2);
        assert!(!f2.is_keyframe);

        // Frame 3: multiple of keyframe_interval (3) -> keyframe
        let f3 = sw_encoder.encode(&frame, false).unwrap();
        assert_eq!(f3.frame_id, 3);
        assert!(f3.is_keyframe);

        // Dynamic bitrate and framerate updates
        sw_encoder.update_bitrate(25_000).unwrap();
        sw_encoder.update_framerate(120).unwrap();
        assert_eq!(sw_encoder.config().target_bitrate_kbps, 25_000);
        assert_eq!(sw_encoder.config().target_fps, 120);
        assert_eq!(sw_encoder.frame_counter(), 3);
    }

    #[test]
    fn test_macos_videotoolbox_encoder_keyframes_and_updates() {
        let mut vt_encoder = MacOsVideoToolboxEncoder::new(EncoderConfig {
            keyframe_interval: 4,
            ..Default::default()
        });
        let frame = sample_frame();

        assert!(vt_encoder.encode(&frame, false).unwrap().is_keyframe); // frame 1
        assert!(!vt_encoder.encode(&frame, false).unwrap().is_keyframe); // frame 2
        assert!(vt_encoder.encode(&frame, true).unwrap().is_keyframe); // frame 3 (forced)
        assert!(vt_encoder.encode(&frame, false).unwrap().is_keyframe); // frame 4 (interval multiple)
        assert!(!vt_encoder.encode(&frame, false).unwrap().is_keyframe); // frame 5

        vt_encoder.update_bitrate(30_000).unwrap();
        vt_encoder.update_framerate(144).unwrap();
        assert_eq!(vt_encoder.config().target_bitrate_kbps, 30_000);
        assert_eq!(vt_encoder.config().target_fps, 144);
        assert_eq!(vt_encoder.frame_counter(), 5);
    }

    #[test]
    fn test_fit_raw_frame_packs_and_decimates() {
        // 4x2 Bgra8 frame at stride 20 (4 padding bytes); pixel (col, row) = [col, row, 0, 255].
        let mut data = Vec::new();
        for row in 0..2u8 {
            for col in 0..4u8 {
                data.extend_from_slice(&[col, row, 0, 255]);
            }
            data.extend_from_slice(&[0xEE; 4]);
        }
        let frame = CapturedFrame {
            frame_index: 7,
            timestamp_us: 9,
            display_id: 1,
            width: 4,
            height: 2,
            stride: 20,
            pixel_format: PixelFormat::Bgra8,
            data,
            is_dmabuf_or_metal_handle: false,
        };
        let packed = fit_raw_frame(&frame, 1024).unwrap();
        assert_eq!((packed.width, packed.height, packed.stride), (4, 2, 16));
        assert_eq!(packed.data.len(), 32);
        assert!(!packed.data.contains(&0xEE));
        let halved = fit_raw_frame(&frame, 8).unwrap();
        assert_eq!((halved.width, halved.height, halved.stride), (2, 1, 8));
        assert_eq!(halved.data, vec![0, 0, 0, 255, 2, 0, 0, 255]);
        assert!(fit_raw_frame(&frame, 3).is_err());
    }

    #[test]
    fn test_raw_encoder_emits_raw_bgra() {
        let frame = CapturedFrame {
            frame_index: 1,
            timestamp_us: 5,
            display_id: 2,
            width: 2,
            height: 2,
            stride: 8,
            pixel_format: PixelFormat::Bgra8,
            data: vec![7; 16],
            is_dmabuf_or_metal_handle: false,
        };
        let mut encoder = RawVideoEncoder::new(EncoderConfig::default());
        let msg = encoder.encode(&frame, false).unwrap();
        assert_eq!(msg.codec, VideoCodec::RawBgra);
        assert_eq!((msg.width, msg.height, msg.stride), (2, 2, 8));
        assert_eq!(msg.payload, vec![7; 16]);
        assert_eq!((msg.frame_id, msg.monitor_id, msg.timestamp_us), (1, 2, 5));
        assert!(msg.is_keyframe);
    }
}
