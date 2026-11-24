//! Client video decoders, including GStreamer-backed AV1 decoding (`vaav1dec` hardware VA-API
//! preferred, `av1dec` software fallback) and zero-overhead `RawBgra` passthrough.

use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt;
use perigee_protocol::{PixelFormat, VideoCodec, VideoFrameMessage};
use tracing::{debug, info, warn};

use crate::render::DecodedFrameSurface;

const PULL_SAMPLE_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(2);
const APPSINK_MAX_BUFFERS: u32 = 2;

static GST_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn ensure_gst_init() -> Result<()> {
    match GST_INIT.get_or_init(|| gst::init().map_err(|err| err.to_string())) {
        Ok(()) => Ok(()),
        Err(err) => bail!("failed to initialize GStreamer: {err}"),
    }
}

/// Decodes incoming protocol [`VideoFrameMessage`] frames into CPU-accessible [`DecodedFrameSurface`] buffers.
pub trait VideoDecoder: Send {
    fn decode(&mut self, frame: &VideoFrameMessage) -> Result<DecodedFrameSurface>;
}

struct ActiveDecoderPipeline {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    appsink: gst_app::AppSink,
}

impl ActiveDecoderPipeline {
    fn shutdown(&self) {
        if let Err(err) = self.appsrc.end_of_stream() {
            debug!(
                error = ?err,
                "GStreamer AV1 decoder appsrc end_of_stream returned error during shutdown"
            );
        }
        if let Err(err) = self.pipeline.set_state(gst::State::Null) {
            warn!(
                error = %err,
                "failed to transition GStreamer AV1 decoder pipeline to Null"
            );
        }
    }
}

/// Live AV1 decoder backed by GStreamer (`vaav1dec` hardware VA-API when available and enabled,
/// with automatic software fallback to `av1dec`).
pub struct GstreamerAv1Decoder {
    element_name: &'static str,
    active: Option<ActiveDecoderPipeline>,
}

impl GstreamerAv1Decoder {
    /// Creates a new GStreamer AV1 decoder, initializing GStreamer and building the decode pipeline.
    pub fn new(prefer_hardware: bool) -> Result<Self> {
        ensure_gst_init()?;
        let element_name = if prefer_hardware && gst::ElementFactory::find("vaav1dec").is_some() {
            "vaav1dec"
        } else {
            "av1dec"
        };
        let active = Self::build_pipeline(element_name)?;
        info!(element = element_name, "Initialized GstreamerAv1Decoder");
        Ok(Self {
            element_name,
            active: Some(active),
        })
    }

    /// Returns the GStreamer decoder element name selected for this instance (`"vaav1dec"` or `"av1dec"`).
    pub fn active_element_name(&self) -> &'static str {
        self.element_name
    }

    fn tear_down_pipeline(&mut self) {
        if let Some(active) = self.active.take() {
            active.shutdown();
        }
    }

    fn build_pipeline(element_name: &'static str) -> Result<ActiveDecoderPipeline> {
        let pipeline = gst::Pipeline::new();

        let appsrc = gst_app::AppSrc::builder()
            .name("perigee-av1-dec-src")
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        let src_caps = gst::Caps::builder("video/x-av1")
            .field("stream-format", "obu-stream")
            .field("alignment", "tu")
            .build();
        appsrc.set_caps(Some(&src_caps));

        let decoder = gst::ElementFactory::make(element_name)
            .name("perigee-av1-dec")
            .build()
            .with_context(|| format!("failed to create GStreamer decoder `{element_name}`"))?;

        let videoconvert = gst::ElementFactory::make("videoconvert")
            .name("perigee-av1-dec-convert")
            .build()
            .context("failed to create GStreamer element `videoconvert`")?;

        let sink_caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build();
        let appsink = gst_app::AppSink::builder()
            .name("perigee-av1-dec-sink")
            .caps(&sink_caps)
            .max_buffers(APPSINK_MAX_BUFFERS)
            .sync(false)
            .build();

        pipeline
            .add_many([
                appsrc.upcast_ref(),
                &decoder,
                &videoconvert,
                appsink.upcast_ref(),
            ])
            .context("failed to add elements to GStreamer AV1 decoder pipeline")?;

        gst::Element::link_many([
            appsrc.upcast_ref(),
            &decoder,
            &videoconvert,
            appsink.upcast_ref(),
        ])
        .context("failed to link GStreamer AV1 decoder pipeline")?;

        pipeline
            .set_state(gst::State::Playing)
            .context("failed to transition GStreamer AV1 decoder pipeline to Playing")?;

        debug!(
            element = element_name,
            "Built GStreamer AV1 decoder pipeline"
        );

        Ok(ActiveDecoderPipeline {
            pipeline,
            appsrc,
            appsink,
        })
    }

    fn ensure_pipeline(&mut self) -> Result<&ActiveDecoderPipeline> {
        if self.active.is_none() {
            let built = Self::build_pipeline(self.element_name)?;
            self.active = Some(built);
        }
        self.active
            .as_ref()
            .ok_or_else(|| anyhow!("GStreamer AV1 decoder pipeline was not initialized"))
    }
}

impl Drop for GstreamerAv1Decoder {
    fn drop(&mut self) {
        self.tear_down_pipeline();
    }
}

impl VideoDecoder for GstreamerAv1Decoder {
    fn decode(&mut self, frame: &VideoFrameMessage) -> Result<DecodedFrameSurface> {
        if frame.codec != VideoCodec::AV1 {
            bail!(
                "GstreamerAv1Decoder only supports VideoCodec::AV1, got {:?}",
                frame.codec
            );
        }
        if frame.payload.is_empty() {
            bail!("cannot decode empty AV1 frame payload");
        }

        let mut gst_buf = gst::Buffer::with_size(frame.payload.len())
            .context("failed to allocate GStreamer buffer for AV1 frame")?;
        {
            let buf_mut = gst_buf
                .get_mut()
                .ok_or_else(|| anyhow!("newly allocated GStreamer buffer is not writable"))?;
            buf_mut.set_pts(gst::ClockTime::from_useconds(frame.timestamp_us));
            let mut mapped = buf_mut
                .map_writable()
                .context("failed to map GStreamer buffer writable")?;
            mapped.as_mut_slice().copy_from_slice(&frame.payload);
        }

        let push_result = {
            let active = self.ensure_pipeline()?;
            active.appsrc.push_buffer(gst_buf)
        };
        if let Err(err) = push_result {
            self.tear_down_pipeline();
            bail!("failed to push AV1 frame to GStreamer appsrc: {err:?}");
        }

        let sample = {
            let active = self
                .active
                .as_ref()
                .ok_or_else(|| anyhow!("GStreamer AV1 decoder pipeline missing after push"))?;
            match active.appsink.try_pull_sample(PULL_SAMPLE_TIMEOUT) {
                Some(sample) => Ok(sample),
                None => {
                    let err = if let Some(bus_err) = poll_bus_error(&active.pipeline) {
                        anyhow!("GStreamer AV1 decoder pipeline error: {bus_err}")
                    } else {
                        anyhow!(
                            "timed out waiting for decoded RGBA frame from GStreamer appsink after {}s",
                            PULL_SAMPLE_TIMEOUT.seconds()
                        )
                    };
                    Err(err)
                }
            }
        };

        let sample = match sample {
            Ok(sample) => sample,
            Err(err) => {
                self.tear_down_pipeline();
                return Err(err);
            }
        };

        let caps = sample
            .caps()
            .ok_or_else(|| anyhow!("GStreamer appsink sample missing caps"))?;
        let video_info = gst_video::VideoInfo::from_caps(caps)
            .context("failed to parse decoded RGBA video caps")?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| anyhow!("GStreamer appsink sample missing buffer"))?;
        let video_frame =
            gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &video_info)
                .map_err(|_| anyhow!("failed to map decoded RGBA video frame readable"))?;

        let width = video_frame.width();
        let height = video_frame.height();
        if width == 0 || height == 0 {
            bail!("decoded RGBA frame has invalid dimensions {width}x{height}");
        }

        let row_bytes = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| anyhow!("decoded frame width {width} overflows RGBA stride"))?;
        let packed_len = row_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| anyhow!("decoded frame size {width}x{height} overflows buffer size"))?;

        let raw_stride = video_frame.plane_stride()[0];
        if raw_stride < 0 {
            bail!("decoded RGBA frame has negative stride {raw_stride}");
        }
        let stride = raw_stride as usize;
        if stride < row_bytes {
            bail!("decoded RGBA frame stride {stride} is smaller than row width {row_bytes}");
        }

        let plane = video_frame
            .plane_data(0)
            .context("decoded RGBA video frame missing plane 0")?;

        let data = if stride == row_bytes {
            if plane.len() < packed_len {
                bail!(
                    "decoded RGBA buffer length {} is smaller than required {packed_len} bytes",
                    plane.len()
                );
            }
            plane[..packed_len].to_vec()
        } else {
            let required = stride
                .checked_mul((height as usize) - 1)
                .and_then(|v| v.checked_add(row_bytes))
                .ok_or_else(|| anyhow!("decoded RGBA strided buffer size overflow"))?;
            if plane.len() < required {
                bail!(
                    "decoded RGBA strided buffer length {} is smaller than required {required} bytes",
                    plane.len()
                );
            }
            let mut packed = Vec::with_capacity(packed_len);
            for row in 0..height as usize {
                let offset = row * stride;
                packed.extend_from_slice(&plane[offset..offset + row_bytes]);
            }
            packed
        };

        Ok(DecodedFrameSurface {
            frame_index: frame.frame_id,
            width,
            height,
            format: PixelFormat::Rgba8,
            data,
            timestamp_us: frame.timestamp_us,
        })
    }
}

/// Client-side multi-codec decoder that passes `VideoCodec::RawBgra` frames through without
/// GStreamer overhead and lazily initializes [`GstreamerAv1Decoder`] on the first `VideoCodec::AV1` frame.
#[derive(Default)]
pub struct ClientVideoDecoder {
    av1_decoder: Option<GstreamerAv1Decoder>,
}

impl ClientVideoDecoder {
    /// Creates a new [`ClientVideoDecoder`] with lazy AV1 initialization.
    pub fn new() -> Self {
        Self::default()
    }
}

impl VideoDecoder for ClientVideoDecoder {
    fn decode(&mut self, frame: &VideoFrameMessage) -> Result<DecodedFrameSurface> {
        match frame.codec {
            VideoCodec::RawBgra => Ok(DecodedFrameSurface {
                frame_index: frame.frame_id,
                width: frame.width,
                height: frame.height,
                format: frame.format,
                data: frame.payload.clone(),
                timestamp_us: frame.timestamp_us,
            }),
            VideoCodec::AV1 => {
                if self.av1_decoder.is_none() {
                    self.av1_decoder = Some(GstreamerAv1Decoder::new(true)?);
                }
                let decoder = self
                    .av1_decoder
                    .as_mut()
                    .ok_or_else(|| anyhow!("AV1 decoder was not initialized"))?;
                decoder.decode(frame)
            }
            VideoCodec::H265 | VideoCodec::H264 => {
                bail!(
                    "unsupported video codec {:?}: only AV1 and RawBgra are supported",
                    frame.codec
                )
            }
        }
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

    #[test]
    fn test_gstreamer_av1_decoder_rejects_empty_and_wrong_codec() {
        let mut decoder =
            GstreamerAv1Decoder::new(false).expect("software av1dec should initialize");
        assert_eq!(decoder.active_element_name(), "av1dec");

        let empty_av1 = VideoFrameMessage {
            frame_id: 1,
            monitor_id: 1,
            timestamp_us: 1000,
            codec: VideoCodec::AV1,
            is_keyframe: true,
            width: 64,
            height: 64,
            stride: 64,
            format: PixelFormat::NV12,
            payload: Vec::new(),
        };
        assert!(decoder.decode(&empty_av1).is_err());

        let wrong_codec = VideoFrameMessage {
            frame_id: 2,
            monitor_id: 1,
            timestamp_us: 2000,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8,
            payload: vec![0xAA; 64],
        };
        assert!(decoder.decode(&wrong_codec).is_err());
    }

    #[test]
    fn test_client_video_decoder_passthrough_raw_and_rejects_h264() {
        let mut decoder = ClientVideoDecoder::new();

        let raw_frame = VideoFrameMessage {
            frame_id: 7,
            monitor_id: 1,
            timestamp_us: 16_666,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: 4,
            height: 2,
            stride: 16,
            format: PixelFormat::Bgra8,
            payload: vec![0x55; 32],
        };
        let surface = decoder
            .decode(&raw_frame)
            .expect("RawBgra frame should pass through");
        assert_eq!(surface.frame_index, 7);
        assert_eq!(surface.width, 4);
        assert_eq!(surface.height, 2);
        assert_eq!(surface.format, PixelFormat::Bgra8);
        assert_eq!(surface.data, vec![0x55; 32]);
        assert_eq!(surface.timestamp_us, 16_666);

        let h264_frame = VideoFrameMessage {
            frame_id: 8,
            monitor_id: 1,
            timestamp_us: 33_333,
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 64,
            height: 64,
            stride: 64,
            format: PixelFormat::NV12,
            payload: vec![0x00, 0x00, 0x00, 0x01, 0x67],
        };
        assert!(decoder.decode(&h264_frame).is_err());
    }
}
