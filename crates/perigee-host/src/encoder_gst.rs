//! GStreamer-backed AV1 video encoder (`vaav1enc` hardware VA-API preferred, `av1enc` software fallback).

use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use tracing::{debug, info, warn};

use perigee_protocol::{PixelFormat, VideoCodec, VideoFrameMessage};

use crate::CapturedFrame;
use crate::encoder::{EncoderConfig, VideoEncoder};

const PULL_SAMPLE_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(2);
const APPSINK_MAX_BUFFERS: u32 = 2;

static GST_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn ensure_gst_init() -> Result<()> {
    match GST_INIT.get_or_init(|| gst::init().map_err(|err| err.to_string())) {
        Ok(()) => Ok(()),
        Err(err) => bail!("failed to initialize GStreamer: {err}"),
    }
}

struct ActivePipeline {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    encoder: gst::Element,
    appsink: gst_app::AppSink,
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    pipeline_frame_index: u64,
    next_pts_ns: u64,
}

impl ActivePipeline {
    fn shutdown(&self) {
        if let Err(err) = self.appsrc.end_of_stream() {
            debug!(error = ?err, "GStreamer AV1 encoder appsrc end_of_stream returned error during shutdown");
        }
        if let Err(err) = self.pipeline.set_state(gst::State::Null) {
            warn!(error = %err, "failed to transition GStreamer AV1 encoder pipeline to Null");
        }
    }
}

/// Live AV1 encoder backed by GStreamer (`vaav1enc` hardware VA-API when available and enabled,
/// with automatic software fallback to `av1enc`).
pub struct GstreamerAv1Encoder {
    config: EncoderConfig,
    use_vaapi: bool,
    frame_counter: u64,
    active: Option<ActivePipeline>,
}

impl GstreamerAv1Encoder {
    /// Creates a new GStreamer AV1 encoder, initializing GStreamer and validating configuration.
    pub fn new(config: EncoderConfig) -> Result<Self> {
        ensure_gst_init()?;
        if config.target_fps == 0 {
            bail!("target_fps must be non-zero");
        }
        if config.target_bitrate_kbps == 0 {
            bail!("target_bitrate_kbps must be non-zero");
        }
        let use_vaapi = config.enable_hardware_accel && Self::is_vaapi_available();
        info!(
            element = if use_vaapi { "vaav1enc" } else { "av1enc" },
            target_fps = config.target_fps,
            target_bitrate_kbps = config.target_bitrate_kbps,
            keyframe_interval = config.keyframe_interval,
            "Initialized GstreamerAv1Encoder"
        );
        Ok(Self {
            config,
            use_vaapi,
            frame_counter: 0,
            active: None,
        })
    }

    /// Returns `true` if GStreamer's VA-API AV1 hardware encoder (`vaav1enc`) is available.
    pub fn is_vaapi_available() -> bool {
        if ensure_gst_init().is_err() {
            return false;
        }
        gst::ElementFactory::find("vaav1enc").is_some()
    }

    /// Returns the GStreamer encoder element name selected for this instance (`"vaav1enc"` or `"av1enc"`).
    pub fn active_element_name(&self) -> &'static str {
        if self.use_vaapi { "vaav1enc" } else { "av1enc" }
    }

    /// Returns a reference to the current encoder configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    fn tear_down_pipeline(&mut self) {
        if let Some(active) = self.active.take() {
            active.shutdown();
        }
    }

    fn fallback_to_software(&mut self, reason: &anyhow::Error) {
        warn!(
            error = %reason,
            "Hardware vaav1enc failed at runtime; falling back to software av1enc"
        );
        self.tear_down_pipeline();
        self.use_vaapi = false;
    }

    fn build_pipeline(
        &self,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> Result<ActivePipeline> {
        let src_caps = build_src_caps(pixel_format, width, height, self.config.target_fps)?;

        let pipeline = gst::Pipeline::new();

        let appsrc = gst_app::AppSrc::builder()
            .name("perigee-av1-src")
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        appsrc.set_caps(Some(&src_caps));

        let use_vapostproc = self.use_vaapi && gst::ElementFactory::find("vapostproc").is_some();
        let converter_name = if use_vapostproc {
            "vapostproc"
        } else {
            "videoconvert"
        };
        let converter = gst::ElementFactory::make(converter_name)
            .name("perigee-av1-convert")
            .build()
            .with_context(|| format!("failed to create GStreamer element `{converter_name}`"))?;

        let enc_input_format = if self.use_vaapi { "NV12" } else { "I420" };
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .name("perigee-av1-conv-caps")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", enc_input_format)
                    .build(),
            )
            .build()
            .context("failed to create GStreamer capsfilter")?;

        let encoder_name = self.active_element_name();
        let encoder = gst::ElementFactory::make(encoder_name)
            .name("perigee-av1-enc")
            .build()
            .with_context(|| format!("failed to create GStreamer encoder `{encoder_name}`"))?;

        configure_encoder_element(&encoder, self.use_vaapi, &self.config);

        let sink_caps = gst::Caps::builder("video/x-av1")
            .field("stream-format", "obu-stream")
            .field("alignment", "tu")
            .build();
        let appsink = gst_app::AppSink::builder()
            .name("perigee-av1-sink")
            .caps(&sink_caps)
            .max_buffers(APPSINK_MAX_BUFFERS)
            .sync(false)
            .build();

        if let Ok(av1parse) = gst::ElementFactory::make("av1parse")
            .name("perigee-av1-parse")
            .build()
        {
            pipeline
                .add_many([
                    appsrc.upcast_ref(),
                    &converter,
                    &capsfilter,
                    &encoder,
                    &av1parse,
                    appsink.upcast_ref(),
                ])
                .context("failed to add elements to GStreamer AV1 encoder pipeline")?;

            gst::Element::link_many([
                appsrc.upcast_ref(),
                &converter,
                &capsfilter,
                &encoder,
                &av1parse,
                appsink.upcast_ref(),
            ])
            .context("failed to link GStreamer AV1 encoder pipeline")?;
        } else {
            pipeline
                .add_many([
                    appsrc.upcast_ref(),
                    &converter,
                    &capsfilter,
                    &encoder,
                    appsink.upcast_ref(),
                ])
                .context("failed to add elements to GStreamer AV1 encoder pipeline")?;

            gst::Element::link_many([
                appsrc.upcast_ref(),
                &converter,
                &capsfilter,
                &encoder,
                appsink.upcast_ref(),
            ])
            .context("failed to link GStreamer AV1 encoder pipeline")?;
        }

        pipeline
            .set_state(gst::State::Playing)
            .context("failed to transition GStreamer AV1 encoder pipeline to Playing")?;

        debug!(
            element = encoder_name,
            converter = converter_name,
            width,
            height,
            ?pixel_format,
            "Built GStreamer AV1 encoder pipeline"
        );

        Ok(ActivePipeline {
            pipeline,
            appsrc,
            encoder,
            appsink,
            width,
            height,
            pixel_format,
            pipeline_frame_index: 0,
            next_pts_ns: 0,
        })
    }

    fn ensure_pipeline(
        &mut self,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> Result<&mut ActivePipeline> {
        let needs_rebuild = match &self.active {
            Some(active) => {
                active.width != width
                    || active.height != height
                    || active.pixel_format != pixel_format
            }
            None => true,
        };
        if needs_rebuild {
            self.tear_down_pipeline();
            let built = self.build_pipeline(width, height, pixel_format)?;
            self.active = Some(built);
        }
        self.active
            .as_mut()
            .ok_or_else(|| anyhow!("GStreamer AV1 encoder pipeline was not initialized"))
    }
    fn encode_once(
        &mut self,
        frame: &CapturedFrame,
        force_keyframe: bool,
    ) -> Result<(Vec<u8>, bool)> {
        let width = frame.width as usize;
        let height = frame.height as usize;
        let stride = frame.stride as usize;
        let row_bytes = width * 4;
        let packed_len = row_bytes * height;

        let target_fps = self.config.target_fps;
        let active = self.ensure_pipeline(frame.width, frame.height, frame.pixel_format)?;

        let frame_idx = active.pipeline_frame_index;
        let (pts_ns, duration_ns) = next_frame_pts_ns(active.next_pts_ns, target_fps);
        let pts = gst::ClockTime::from_nseconds(pts_ns);
        let duration = gst::ClockTime::from_nseconds(duration_ns);

        if force_keyframe {
            let event = gst_video::DownstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            if let Some(src_pad) = active.appsrc.static_pad("src") {
                src_pad.push_event(event);
            } else {
                active.appsrc.send_event(event);
            }
        }

        let mut gst_buf = gst::Buffer::with_size(packed_len)
            .context("failed to allocate GStreamer buffer for video frame")?;
        {
            let buf_mut = gst_buf
                .get_mut()
                .ok_or_else(|| anyhow!("newly allocated GStreamer buffer is not writable"))?;
            buf_mut.set_pts(pts);
            buf_mut.set_duration(duration);

            let mut mapped = buf_mut
                .map_writable()
                .context("failed to map GStreamer buffer writable")?;
            let dst = mapped.as_mut_slice();
            if stride == row_bytes {
                dst.copy_from_slice(&frame.data[..packed_len]);
            } else {
                for row in 0..height {
                    let src_offset = row * stride;
                    let dst_offset = row * row_bytes;
                    dst[dst_offset..dst_offset + row_bytes]
                        .copy_from_slice(&frame.data[src_offset..src_offset + row_bytes]);
                }
            }
        }

        if let Err(err) = active.appsrc.push_buffer(gst_buf) {
            self.tear_down_pipeline();
            return Err(anyhow!(
                "failed to push video frame to GStreamer appsrc: {err:?}"
            ));
        }
        active.pipeline_frame_index += 1;
        active.next_pts_ns = pts_ns;

        let Some(sample) = active.appsink.try_pull_sample(PULL_SAMPLE_TIMEOUT) else {
            let err = if let Some(bus_err) = poll_bus_error(&active.pipeline) {
                anyhow!("GStreamer AV1 encoder pipeline error: {bus_err}")
            } else {
                anyhow!(
                    "timed out waiting for encoded AV1 frame from GStreamer appsink after {}s",
                    PULL_SAMPLE_TIMEOUT.seconds()
                )
            };
            self.tear_down_pipeline();
            return Err(err);
        };

        let Some(out_buf) = sample.buffer() else {
            self.tear_down_pipeline();
            bail!("GStreamer appsink sample missing buffer");
        };
        let is_keyframe = frame_idx == 0 || !out_buf.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let map = match out_buf
            .map_readable()
            .context("failed to map encoded AV1 buffer readable")
        {
            Ok(map) => map,
            Err(err) => {
                self.tear_down_pipeline();
                return Err(err);
            }
        };
        let payload = map.as_slice().to_vec();

        Ok((payload, is_keyframe))
    }

    fn encode_with_keyframe_retry(
        &mut self,
        frame: &CapturedFrame,
        force_keyframe: bool,
    ) -> Result<(Vec<u8>, bool)> {
        let (mut payload, mut is_keyframe) = self.encode_once(frame, force_keyframe)?;

        // Older GStreamer aom plugins (e.g. 1.22.0) ignore GstForceKeyUnit; rebuild the
        // pipeline on demand so forced keyframes always emit a true AV1 keyframe + headers.
        if force_keyframe && !is_keyframe {
            debug!(
                element = self.active_element_name(),
                "GStreamer encoder ignored GstForceKeyUnit; rebuilding pipeline for forced keyframe"
            );
            self.tear_down_pipeline();
            (payload, is_keyframe) = self.encode_once(frame, true)?;
        }

        Ok((payload, is_keyframe))
    }
}

impl Drop for GstreamerAv1Encoder {
    fn drop(&mut self) {
        self.tear_down_pipeline();
    }
}

impl VideoEncoder for GstreamerAv1Encoder {
    fn encode(&mut self, frame: &CapturedFrame, force_keyframe: bool) -> Result<VideoFrameMessage> {
        validate_frame(frame)?;

        let (payload, is_keyframe) = match self.encode_with_keyframe_retry(frame, force_keyframe) {
            Ok(encoded) => encoded,
            Err(err) if self.use_vaapi => {
                self.fallback_to_software(&err);
                self.encode_with_keyframe_retry(frame, force_keyframe)?
            }
            Err(err) => return Err(err),
        };

        self.frame_counter += 1;

        Ok(VideoFrameMessage {
            frame_id: self.frame_counter,
            monitor_id: frame.display_id,
            timestamp_us: frame.timestamp_us,
            codec: VideoCodec::AV1,
            is_keyframe,
            width: frame.width,
            height: frame.height,
            stride: frame.width,
            format: PixelFormat::NV12,
            payload,
        })
    }

    fn update_bitrate(&mut self, target_bitrate_kbps: u32) -> Result<()> {
        if target_bitrate_kbps == 0 {
            bail!("target_bitrate_kbps must be non-zero");
        }
        self.config.target_bitrate_kbps = target_bitrate_kbps;
        if let Some(active) = &self.active {
            apply_bitrate_property(&active.encoder, self.use_vaapi, target_bitrate_kbps);
        }
        debug!(
            target_bitrate_kbps,
            element = self.active_element_name(),
            "Updated GStreamer AV1 target bitrate"
        );
        Ok(())
    }

    fn update_framerate(&mut self, target_fps: u32) -> Result<()> {
        if target_fps == 0 {
            bail!("target_fps must be non-zero");
        }
        self.config.target_fps = target_fps;
        if let Some(active) = &self.active {
            let src_caps =
                build_src_caps(active.pixel_format, active.width, active.height, target_fps)?;
            active.appsrc.set_caps(Some(&src_caps));
        }
        debug!(
            target_fps,
            element = self.active_element_name(),
            "Updated GStreamer AV1 target framerate"
        );
        Ok(())
    }
}

fn next_frame_pts_ns(current_pts_ns: u64, target_fps: u32) -> (u64, u64) {
    let duration_ns = 1_000_000_000u64.div_ceil(target_fps.max(1) as u64);
    let pts_ns = current_pts_ns.saturating_add(duration_ns);
    (pts_ns, duration_ns)
}

fn build_src_caps(
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    target_fps: u32,
) -> Result<gst::Caps> {
    let input_gst_format = match pixel_format {
        PixelFormat::Bgra8 => "BGRA",
        PixelFormat::Rgba8 => "RGBA",
        other => bail!("unsupported input pixel format for AV1 encoder: {other:?}"),
    };
    Ok(gst::Caps::builder("video/x-raw")
        .field("format", input_gst_format)
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(target_fps as i32, 1))
        .build())
}

fn validate_frame(frame: &CapturedFrame) -> Result<()> {
    if !matches!(frame.pixel_format, PixelFormat::Bgra8 | PixelFormat::Rgba8) {
        bail!(
            "AV1 encoder expects Bgra8 or Rgba8 input, not {:?}",
            frame.pixel_format
        );
    }
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    if width == 0 || height == 0 {
        bail!(
            "AV1 encoder requires non-zero frame dimensions, got {}x{}",
            frame.width,
            frame.height
        );
    }
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| anyhow!("frame width {} overflows byte stride", frame.width))?;
    if stride < row_bytes {
        bail!(
            "frame stride {} is smaller than width * 4 ({row_bytes})",
            frame.stride
        );
    }
    let required_bytes = stride
        .checked_mul(height - 1)
        .and_then(|v| v.checked_add(row_bytes))
        .ok_or_else(|| {
            anyhow!(
                "frame size {}x{} overflows buffer bounds",
                frame.width,
                frame.height
            )
        })?;
    if frame.data.len() < required_bytes {
        bail!(
            "frame data length {} is smaller than required {} bytes for {}x{} stride {}",
            frame.data.len(),
            required_bytes,
            frame.width,
            frame.height,
            frame.stride
        );
    }
    Ok(())
}

fn configure_encoder_element(encoder: &gst::Element, use_vaapi: bool, config: &EncoderConfig) {
    let keyframe_interval = config.keyframe_interval.max(1);
    if encoder
        .find_property("min-force-key-unit-interval")
        .is_some()
    {
        encoder.set_property("min-force-key-unit-interval", 0u64);
    }

    if use_vaapi {
        apply_bitrate_property(encoder, true, config.target_bitrate_kbps);
        if encoder.find_property("key-int-max").is_some() {
            encoder.set_property("key-int-max", keyframe_interval);
        }
        if encoder.find_property("rate-control").is_some() {
            encoder.set_property_from_str("rate-control", "cbr");
        }
        if encoder.find_property("target-usage").is_some() {
            encoder.set_property("target-usage", 7u32);
        }
    } else {
        apply_bitrate_property(encoder, false, config.target_bitrate_kbps);
        if encoder.find_property("usage-profile").is_some() {
            encoder.set_property_from_str("usage-profile", "realtime");
        }
        if encoder.find_property("end-usage").is_some() {
            encoder.set_property_from_str("end-usage", "cbr");
        }
        if encoder.find_property("cpu-used").is_some() {
            encoder.set_property("cpu-used", 8i32);
        }
        if encoder.find_property("lag-in-frames").is_some() {
            encoder.set_property("lag-in-frames", 0u32);
        }
        if encoder.find_property("row-mt").is_some() {
            encoder.set_property("row-mt", true);
        }
        if encoder.find_property("keyframe-max-dist").is_some() {
            encoder.set_property(
                "keyframe-max-dist",
                keyframe_interval.min(i32::MAX as u32) as i32,
            );
        }
    }
}

fn apply_bitrate_property(encoder: &gst::Element, use_vaapi: bool, target_bitrate_kbps: u32) {
    let prop = if use_vaapi {
        "bitrate"
    } else {
        "target-bitrate"
    };
    if encoder.find_property(prop).is_some() {
        encoder.set_property(prop, target_bitrate_kbps);
    }
}

fn poll_bus_error(pipeline: &gst::Pipeline) -> Option<String> {
    let bus = pipeline.bus()?;
    while let Some(msg) = bus.pop() {
        if let gst::MessageView::Error(err) = msg.view() {
            return Some(format!("{} (debug: {:?})", err.error(), err.debug()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_bgra_frame(
        frame_index: u64,
        width: u32,
        height: u32,
        stride: u32,
        seed: u8,
    ) -> CapturedFrame {
        let mut data = vec![0u8; (stride as usize) * (height as usize)];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let offset = y * (stride as usize) + x * 4;
                data[offset] = ((x as u8).wrapping_add(seed)).wrapping_mul(3);
                data[offset + 1] = ((y as u8).wrapping_add(seed)).wrapping_mul(5);
                data[offset + 2] = seed.wrapping_mul(17);
                data[offset + 3] = 255;
            }
        }
        CapturedFrame {
            frame_index,
            timestamp_us: frame_index * 33_333,
            display_id: 1,
            width,
            height,
            stride,
            pixel_format: PixelFormat::Bgra8,
            data,
            is_dmabuf_or_metal_handle: false,
        }
    }

    #[test]
    fn test_gstreamer_av1_encoder_encodes_keyframe_and_delta() {
        let mut encoder = GstreamerAv1Encoder::new(EncoderConfig {
            codec: VideoCodec::AV1,
            width: 128,
            height: 128,
            target_fps: 30,
            target_bitrate_kbps: 1000,
            enable_hardware_accel: false,
            keyframe_interval: 60,
        })
        .expect("GstreamerAv1Encoder::new should succeed with software fallback");
        assert_eq!(encoder.active_element_name(), "av1enc");

        let f1 = synthetic_bgra_frame(1, 128, 128, 128 * 4, 1);
        let f2 = synthetic_bgra_frame(2, 128, 128, 128 * 4, 2);
        let f3 = synthetic_bgra_frame(3, 128, 128, 128 * 4, 3);

        let m1 = encoder.encode(&f1, true).expect("frame 1 should encode");
        assert_eq!(m1.codec, VideoCodec::AV1);
        assert_eq!(m1.format, PixelFormat::NV12);
        assert_eq!((m1.width, m1.height, m1.stride), (128, 128, 128));
        assert!(!m1.payload.is_empty());
        assert!(m1.is_keyframe, "frame 1 should be a keyframe");

        let m2 = encoder.encode(&f2, false).expect("frame 2 should encode");
        assert_eq!(m2.codec, VideoCodec::AV1);
        assert!(!m2.payload.is_empty());

        let m3 = encoder.encode(&f3, true).expect("frame 3 should encode");
        assert_eq!(m3.codec, VideoCodec::AV1);
        assert!(!m3.payload.is_empty());
        assert!(
            m3.is_keyframe,
            "frame 3 with force_keyframe should be a keyframe"
        );
    }

    #[test]
    fn test_gstreamer_av1_encoder_handles_resize_and_validation() {
        let mut encoder = GstreamerAv1Encoder::new(EncoderConfig {
            codec: VideoCodec::AV1,
            width: 128,
            height: 128,
            target_fps: 30,
            target_bitrate_kbps: 1000,
            enable_hardware_accel: false,
            keyframe_interval: 60,
        })
        .expect("GstreamerAv1Encoder::new should succeed");

        // Encode 128x128 with padded stride to exercise row packing too.
        let frame_128 = synthetic_bgra_frame(1, 128, 128, 128 * 4 + 16, 10);
        let msg_128 = encoder
            .encode(&frame_128, true)
            .expect("128x128 should encode");
        assert_eq!((msg_128.width, msg_128.height), (128, 128));
        assert_eq!(msg_128.codec, VideoCodec::AV1);
        assert!(!msg_128.payload.is_empty());

        // Resize to 160x128.
        let frame_160 = synthetic_bgra_frame(2, 160, 128, 160 * 4, 20);
        let msg_160 = encoder
            .encode(&frame_160, false)
            .expect("160x128 should encode");
        assert_eq!((msg_160.width, msg_160.height), (160, 128));
        assert_eq!(msg_160.codec, VideoCodec::AV1);
        assert!(!msg_160.payload.is_empty());

        // Validation: 0-width frame fails.
        let zero_width = CapturedFrame {
            width: 0,
            ..frame_128.clone()
        };
        assert!(encoder.encode(&zero_width, false).is_err());

        // Validation: truncated frame data fails.
        let mut truncated = frame_128.clone();
        truncated.data.truncate(100);
        assert!(encoder.encode(&truncated, false).is_err());

        // Validation: zero bitrate update fails; non-zero succeeds.
        assert!(encoder.update_bitrate(0).is_err());
        encoder
            .update_bitrate(2000)
            .expect("non-zero bitrate update should succeed");
        assert!(encoder.update_framerate(0).is_err());
        encoder
            .update_framerate(60)
            .expect("non-zero framerate update should succeed");
    }

    #[test]
    fn test_gstreamer_av1_encoder_falls_back_to_software_and_recovers_after_error() {
        let mut encoder = GstreamerAv1Encoder::new(EncoderConfig {
            codec: VideoCodec::AV1,
            width: 128,
            height: 128,
            target_fps: 30,
            target_bitrate_kbps: 1000,
            enable_hardware_accel: false,
            keyframe_interval: 60,
        })
        .expect("GstreamerAv1Encoder::new should succeed");

        encoder.use_vaapi = true;
        encoder.fallback_to_software(&anyhow!("simulated vaav1enc failure"));
        assert_eq!(encoder.active_element_name(), "av1enc");

        let frame = synthetic_bgra_frame(1, 128, 128, 128 * 4, 7);
        let msg = encoder
            .encode(&frame, true)
            .expect("software fallback encode should succeed");
        assert_eq!(msg.codec, VideoCodec::AV1);
        assert!(!msg.payload.is_empty());
        assert!(msg.is_keyframe);
        assert!(encoder.active.is_some());

        encoder
            .active
            .as_ref()
            .expect("active pipeline should exist")
            .appsrc
            .end_of_stream()
            .expect("end_of_stream should succeed");

        let fault_err = encoder.encode(&frame, false);
        assert!(fault_err.is_err(), "encode on faulted pipeline should fail");
        assert!(
            encoder.active.is_none(),
            "failed encode should tear down active pipeline"
        );

        let recovered = encoder
            .encode(&frame, true)
            .expect("subsequent encode should rebuild pipeline and succeed");
        assert_eq!(recovered.codec, VideoCodec::AV1);
        assert!(!recovered.payload.is_empty());
        assert!(recovered.is_keyframe);
        assert!(encoder.active.is_some());
    }

    #[test]
    fn test_gstreamer_av1_encoder_pts_is_monotonic_across_framerate_increase() {
        let (pts1, dur1) = next_frame_pts_ns(0, 30);
        let (pts2, dur2) = next_frame_pts_ns(pts1, 30);
        let (pts3, dur3) = next_frame_pts_ns(pts2, 60);
        let (pts4, dur4) = next_frame_pts_ns(pts3, 60);
        assert!(pts1 > 0 && dur1 > 0);
        assert!(pts2 > pts1 && dur2 == dur1);
        assert!(pts3 > pts2 && dur3 > 0 && dur3 < dur1);
        assert!(pts4 > pts3 && dur4 == dur3);

        let mut encoder = GstreamerAv1Encoder::new(EncoderConfig {
            codec: VideoCodec::AV1,
            width: 128,
            height: 128,
            target_fps: 30,
            target_bitrate_kbps: 1000,
            enable_hardware_accel: false,
            keyframe_interval: 60,
        })
        .expect("GstreamerAv1Encoder::new should succeed");

        let f1 = synthetic_bgra_frame(1, 128, 128, 128 * 4, 11);
        let f2 = synthetic_bgra_frame(2, 128, 128, 128 * 4, 12);
        let f3 = synthetic_bgra_frame(3, 128, 128, 128 * 4, 13);
        let f4 = synthetic_bgra_frame(4, 128, 128, 128 * 4, 14);

        encoder
            .encode(&f1, true)
            .expect("frame 1 at 30 fps should encode");
        let pts_after_1 = encoder
            .active
            .as_ref()
            .expect("active pipeline should exist")
            .next_pts_ns;
        assert!(pts_after_1 > 0);

        encoder
            .encode(&f2, false)
            .expect("frame 2 at 30 fps should encode");
        let pts_after_2 = encoder
            .active
            .as_ref()
            .expect("active pipeline should exist")
            .next_pts_ns;
        assert!(pts_after_2 > pts_after_1);

        encoder
            .update_framerate(60)
            .expect("update_framerate(60) should succeed");

        encoder
            .encode(&f3, false)
            .expect("frame 3 at 60 fps should encode");
        let pts_after_3 = encoder
            .active
            .as_ref()
            .expect("active pipeline should exist")
            .next_pts_ns;
        assert!(pts_after_3 > pts_after_2);

        encoder
            .encode(&f4, false)
            .expect("frame 4 at 60 fps should encode");
        let pts_after_4 = encoder
            .active
            .as_ref()
            .expect("active pipeline should exist")
            .next_pts_ns;
        assert!(pts_after_4 > pts_after_3);
    }
}
