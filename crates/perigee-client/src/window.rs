//! Minimal `wgpu` (`30.0.1`) + `winit` (`0.30.13`) bare viewer window for `perigee-client`.
//!
//! Presents incoming [`DecodedFrameSurface`] (`Rgba8`/`Bgra8`) frames on screen with
//! aspect-preserving letterboxing, window resize handling, and clean close/disconnect signaling.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use perigee_protocol::PixelFormat;
use tokio::sync::mpsc::error::TryRecvError;
use tracing::{debug, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::render::{DecodedFrameSurface, WgpuRenderPipeline};

/// Minimum inner width in logical pixels enforced on the viewer window.
pub const MIN_VIEWER_WIDTH: u32 = 320;

/// Minimum inner height in logical pixels enforced on the viewer window.
pub const MIN_VIEWER_HEIGHT: u32 = 240;

/// Maximum number of characters retained from the caller-supplied base window title.
const MAX_TITLE_BASE_CHARS: usize = 120;

/// Frame-polling cadence while the window is idle with no OS events.
const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Configuration for the bare `wgpu` + `winit` viewer window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerWindowConfig {
    /// Base title displayed in the window title bar.
    pub title: String,
    /// Initial inner window width in logical pixels.
    pub initial_width: u32,
    /// Initial inner window height in logical pixels.
    pub initial_height: u32,
}

impl Default for ViewerWindowConfig {
    fn default() -> Self {
        Self {
            title: "Perigee Viewer".to_string(),
            initial_width: 1280,
            initial_height: 720,
        }
    }
}

/// Letterboxed viewport rectangle in surface pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportRect {
    /// Horizontal offset from the left edge of the surface in pixels.
    pub x: f32,
    /// Vertical offset from the top edge of the surface in pixels.
    pub y: f32,
    /// Rendered viewport width in pixels.
    pub width: f32,
    /// Rendered viewport height in pixels.
    pub height: f32,
}

/// Computes an aspect-preserving, centered viewport rectangle for `frame_w x frame_h`
/// inside a `surface_w x surface_h` render target.
///
/// Returns a zeroed [`ViewportRect`] if any dimension is zero.
pub fn compute_letterbox_viewport(
    surface_w: u32,
    surface_h: u32,
    frame_w: u32,
    frame_h: u32,
) -> ViewportRect {
    if surface_w == 0 || surface_h == 0 || frame_w == 0 || frame_h == 0 {
        return ViewportRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }

    let sw = f64::from(surface_w);
    let sh = f64::from(surface_h);
    let fw = f64::from(frame_w);
    let fh = f64::from(frame_h);

    let scale = (sw / fw).min(sh / fh);
    let sw_f32 = surface_w as f32;
    let sh_f32 = surface_h as f32;

    let width = ((fw * scale) as f32).clamp(0.0, sw_f32);
    let height = ((fh * scale) as f32).clamp(0.0, sh_f32);
    let mut x = ((sw_f32 - width) * 0.5).max(0.0);
    let mut y = ((sh_f32 - height) * 0.5).max(0.0);

    if x + width > sw_f32 {
        x = (sw_f32 - width).max(0.0);
    }
    if y + height > sh_f32 {
        y = (sh_f32 - height).max(0.0);
    }

    ViewportRect {
        x,
        y,
        width,
        height,
    }
}

/// Formats the viewer window title, truncating overly long base strings safely on
/// Unicode scalar boundaries and appending ` (WxH)` when a frame size is known.
pub fn format_window_title(base: &str, frame_size: Option<(u32, u32)>) -> String {
    let effective_base = if base.trim().is_empty() {
        "Perigee Viewer"
    } else {
        base
    };

    let truncated: String = if effective_base.chars().count() > MAX_TITLE_BASE_CHARS {
        let mut s: String = effective_base
            .chars()
            .take(MAX_TITLE_BASE_CHARS.saturating_sub(3))
            .collect();
        s.push_str("...");
        s
    } else {
        effective_base.to_string()
    };

    match frame_size {
        Some((w, h)) if w > 0 && h > 0 => format!("{truncated} ({w}x{h})"),
        _ => truncated,
    }
}

/// Recovery policy for non-presentable `wgpu` surface acquisition outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRecoveryAction {
    /// Reconfigure the surface and immediately request another redraw so static streams stay fresh.
    ReconfigureAndRedraw,
    /// Skip this frame without requesting an extra redraw.
    Skip,
    /// Propagate a fatal surface validation error.
    Error,
}

/// Classifies a `wgpu` surface acquisition outcome into its recovery action.
fn classify_surface_recovery(outcome: &wgpu::CurrentSurfaceTexture) -> SurfaceRecoveryAction {
    match outcome {
        wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
            SurfaceRecoveryAction::ReconfigureAndRedraw
        }
        wgpu::CurrentSurfaceTexture::Timeout
        | wgpu::CurrentSurfaceTexture::Occluded
        | wgpu::CurrentSurfaceTexture::Success(_)
        | wgpu::CurrentSurfaceTexture::Suboptimal(_) => SurfaceRecoveryAction::Skip,
        wgpu::CurrentSurfaceTexture::Validation => SurfaceRecoveryAction::Error,
    }
}

/// Minimal thread-parking waker used to drive `wgpu` adapter/device futures synchronously
/// without requiring or conflicting with a Tokio runtime on the UI thread.
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on_future<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

struct UploadedFrameTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    format: PixelFormat,
}

struct GpuState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame_texture: Option<UploadedFrameTexture>,
}

impl GpuState {
    fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("Failed to create wgpu surface for viewer window")?;

        let adapter = block_on_future(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .context("Failed to find a compatible GPU adapter for viewer window")?;

        let (device, queue) = block_on_future(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("perigee_viewer_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .context("Failed to create wgpu device for viewer window")?;

        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("wgpu surface reported no supported texture formats"))?;

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };

        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);

        let mut surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| anyhow!("wgpu surface is not compatible with selected GPU adapter"))?;
        surface_config.format = surface_format;
        surface_config.present_mode = present_mode;
        surface_config.desired_maximum_frame_latency = 2;
        surface_config.alpha_mode = alpha_mode;
        surface.configure(&device, &surface_config);

        let shader_desc = WgpuRenderPipeline::validate_default_shader()
            .context("Built-in VIDEO_SURFACE_SHADER_WGSL validation failed")?;
        let shader = device.create_shader_module(shader_desc);

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("perigee_frame_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("perigee_frame_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("perigee_viewer_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("perigee_viewer_render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            window,
            surface,
            device,
            queue,
            surface_config,
            render_pipeline,
            bind_group_layout,
            sampler,
            frame_texture: None,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    fn upload_frame(&mut self, frame: &DecodedFrameSurface, base_title: &str) -> Result<()> {
        if !matches!(frame.format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
            warn!(
                format = ?frame.format,
                "Viewer window only supports Rgba8/Bgra8 frames; skipping unsupported frame"
            );
            return Ok(());
        }

        let layout = frame.validate()?;
        let Some(plane) = layout.planes.first() else {
            return Ok(());
        };

        let needs_recreate = match self.frame_texture {
            Some(ref existing) => {
                existing.width != frame.width
                    || existing.height != frame.height
                    || existing.format != frame.format
            }
            None => true,
        };

        if needs_recreate {
            let desc =
                WgpuRenderPipeline::texture_descriptor(plane, Some("perigee_viewer_frame_texture"));
            let texture = self.device.create_texture(&desc);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("perigee_viewer_frame_bind_group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.frame_texture = Some(UploadedFrameTexture {
                texture,
                bind_group,
                width: frame.width,
                height: frame.height,
                format: frame.format,
            });
            self.window.set_title(&format_window_title(
                base_title,
                Some((frame.width, frame.height)),
            ));
        }

        if let Some(ref uploaded) = self.frame_texture {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &uploaded.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data[..plane.unpadded_byte_size],
                plane.texel_copy_buffer_layout(false),
                plane.extent_3d(),
            );
        }

        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        if self.surface_config.width == 0 || self.surface_config.height == 0 {
            return Ok(());
        }

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            other => match classify_surface_recovery(&other) {
                SurfaceRecoveryAction::ReconfigureAndRedraw => {
                    self.surface.configure(&self.device, &self.surface_config);
                    self.window.request_redraw();
                    return Ok(());
                }
                SurfaceRecoveryAction::Skip => {
                    return Ok(());
                }
                SurfaceRecoveryAction::Error => {
                    return Err(anyhow!(
                        "wgpu surface validation error while acquiring frame"
                    ));
                }
            },
        };

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("perigee_viewer_render_encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("perigee_viewer_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            if let Some(ref uploaded) = self.frame_texture {
                let vp = compute_letterbox_viewport(
                    self.surface_config.width,
                    self.surface_config.height,
                    uploaded.width,
                    uploaded.height,
                );
                if vp.width > 0.0 && vp.height > 0.0 {
                    render_pass.set_viewport(vp.x, vp.y, vp.width, vp.height, 0.0, 1.0);
                    render_pass.set_pipeline(&self.render_pipeline);
                    render_pass.set_bind_group(0, &uploaded.bind_group, &[]);
                    render_pass.draw(0..3, 0..1);
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        self.queue.present(output);
        Ok(())
    }
}

struct ViewerApp {
    config: ViewerWindowConfig,
    frame_rx: tokio::sync::mpsc::Receiver<DecodedFrameSurface>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    gpu: Option<GpuState>,
    pending_frame: Option<DecodedFrameSurface>,
    modifiers: ModifiersState,
    disconnected: bool,
    fatal_error: Option<anyhow::Error>,
}

impl ViewerApp {
    fn signal_shutdown_and_exit(&self, event_loop: &ActiveEventLoop) {
        let _ = self.shutdown_tx.send(true);
        event_loop.exit();
    }

    fn drain_latest_frame(&mut self) -> bool {
        let mut latest: Option<DecodedFrameSurface> = None;
        loop {
            match self.frame_rx.try_recv() {
                Ok(frame) => {
                    latest = Some(frame);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    break;
                }
            }
        }

        let Some(frame) = latest else {
            return false;
        };

        if let Some(ref mut gpu) = self.gpu {
            if let Err(err) = gpu.upload_frame(&frame, &self.config.title) {
                warn!(error = %err, "Failed to upload decoded frame to viewer texture");
                return false;
            }
            true
        } else {
            self.pending_frame = Some(frame);
            false
        }
    }
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let initial_w = self.config.initial_width.max(MIN_VIEWER_WIDTH);
        let initial_h = self.config.initial_height.max(MIN_VIEWER_HEIGHT);
        let attrs = Window::default_attributes()
            .with_title(format_window_title(&self.config.title, None))
            .with_inner_size(LogicalSize::new(initial_w, initial_h))
            .with_min_inner_size(LogicalSize::new(MIN_VIEWER_WIDTH, MIN_VIEWER_HEIGHT));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                self.fatal_error = Some(anyhow!("Failed to create viewer window: {err}"));
                self.signal_shutdown_and_exit(event_loop);
                return;
            }
        };

        match GpuState::new(Arc::clone(&window)) {
            Ok(mut gpu) => {
                if let Some(frame) = self.pending_frame.take()
                    && let Err(err) = gpu.upload_frame(&frame, &self.config.title)
                {
                    warn!(error = %err, "Failed to upload initial pending frame");
                }
                gpu.window.request_redraw();
                self.gpu = Some(gpu);
            }
            Err(err) => {
                self.fatal_error = Some(err);
                self.signal_shutdown_and_exit(event_loop);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                debug!("Viewer window close requested");
                self.signal_shutdown_and_exit(event_loop);
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width == 0 || new_size.height == 0 {
                    return;
                }
                if let Some(ref mut gpu) = self.gpu {
                    gpu.resize(new_size.width, new_size.height);
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state == ElementState::Pressed {
                    let is_escape = matches!(key_event.logical_key, Key::Named(NamedKey::Escape));
                    let is_ctrl_q = matches!(
                        key_event.logical_key,
                        Key::Character(ref ch) if ch.eq_ignore_ascii_case("q")
                    ) && self.modifiers.control_key();
                    if is_escape || is_ctrl_q {
                        debug!("Viewer window exit shortcut triggered");
                        self.signal_shutdown_and_exit(event_loop);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let _ = self.drain_latest_frame();
                if self.disconnected {
                    debug!("Viewer frame channel disconnected; closing window");
                    self.signal_shutdown_and_exit(event_loop);
                    return;
                }
                if let Some(ref mut gpu) = self.gpu
                    && let Err(err) = gpu.render()
                {
                    self.fatal_error = Some(err);
                    self.signal_shutdown_and_exit(event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let got_frame = self.drain_latest_frame();
        if self.disconnected {
            debug!("Viewer frame channel disconnected; closing window");
            self.signal_shutdown_and_exit(event_loop);
            return;
        }
        if got_frame && let Some(ref gpu) = self.gpu {
            gpu.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME_POLL_INTERVAL));
    }
}

/// Runs the bare `wgpu` + `winit` viewer window on the calling thread until the user closes it,
/// presses `Escape` / `Ctrl+Q`, or `frame_rx` disconnects.
pub fn run_viewer_window(
    config: ViewerWindowConfig,
    frame_rx: tokio::sync::mpsc::Receiver<DecodedFrameSurface>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    let event_loop = EventLoop::new().context("Failed to create winit event loop")?;
    event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME_POLL_INTERVAL));

    let mut app = ViewerApp {
        config,
        frame_rx,
        shutdown_tx: shutdown_tx.clone(),
        gpu: None,
        pending_frame: None,
        modifiers: ModifiersState::empty(),
        disconnected: false,
        fatal_error: None,
    };

    let run_result = event_loop
        .run_app(&mut app)
        .context("winit event loop exited with error");
    let _ = shutdown_tx.send(true);

    if let Some(err) = app.fatal_error {
        return Err(err);
    }
    run_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_viewport_valid(vp: ViewportRect, surface_w: u32, surface_h: u32) {
        assert!(vp.x.is_finite() && vp.y.is_finite());
        assert!(vp.width.is_finite() && vp.height.is_finite());
        assert!(vp.x >= 0.0, "x must be non-negative: {}", vp.x);
        assert!(vp.y >= 0.0, "y must be non-negative: {}", vp.y);
        assert!(vp.width >= 0.0, "width must be non-negative: {}", vp.width);
        assert!(
            vp.height >= 0.0,
            "height must be non-negative: {}",
            vp.height
        );
        assert!(
            vp.width <= surface_w as f32,
            "width {} exceeds surface {}",
            vp.width,
            surface_w
        );
        assert!(
            vp.height <= surface_h as f32,
            "height {} exceeds surface {}",
            vp.height,
            surface_h
        );
        assert!(
            vp.x + vp.width <= surface_w as f32 + 1e-4,
            "x + width ({}) exceeds surface {}",
            vp.x + vp.width,
            surface_w
        );
        assert!(
            vp.y + vp.height <= surface_h as f32 + 1e-4,
            "y + height ({}) exceeds surface {}",
            vp.y + vp.height,
            surface_h
        );
    }

    #[test]
    fn test_compute_letterbox_viewport_narrow_and_ultrawide() {
        // Narrowest supported window (320x240) displaying a 5120x1440 ultrawide frame.
        let ultrawide = compute_letterbox_viewport(MIN_VIEWER_WIDTH, MIN_VIEWER_HEIGHT, 5120, 1440);
        assert_viewport_valid(ultrawide, MIN_VIEWER_WIDTH, MIN_VIEWER_HEIGHT);
        assert!((ultrawide.width - 320.0).abs() < 1e-4);
        assert!((ultrawide.height - 90.0).abs() < 1e-4);
        assert!((ultrawide.x - 0.0).abs() < 1e-4);
        assert!((ultrawide.y - 75.0).abs() < 1e-4);
        let expected_aspect = 5120.0_f32 / 1440.0_f32;
        assert!(((ultrawide.width / ultrawide.height) - expected_aspect).abs() < 1e-4);

        // 16:9 surface (1280x720) displaying a 16:9 frame (1920x1080) -> fills cleanly.
        let hd = compute_letterbox_viewport(1280, 720, 1920, 1080);
        assert_viewport_valid(hd, 1280, 720);
        assert!((hd.width - 1280.0).abs() < 1e-4);
        assert!((hd.height - 720.0).abs() < 1e-4);
        assert!((hd.x - 0.0).abs() < 1e-4);
        assert!((hd.y - 0.0).abs() < 1e-4);

        // Landscape surface (800x600) displaying a portrait frame (600x800) -> pillarboxed.
        let portrait = compute_letterbox_viewport(800, 600, 600, 800);
        assert_viewport_valid(portrait, 800, 600);
        assert!((portrait.width - 450.0).abs() < 1e-4);
        assert!((portrait.height - 600.0).abs() < 1e-4);
        assert!((portrait.x - 175.0).abs() < 1e-4);
        assert!((portrait.y - 0.0).abs() < 1e-4);
        assert!(((portrait.width / portrait.height) - (600.0_f32 / 800.0_f32)).abs() < 1e-4);

        // Zero dimensions -> zero rect, no NaN/inf, no panic.
        for (sw, sh, fw, fh) in [(0, 0, 1920, 1080), (1280, 720, 0, 0), (0, 0, 0, 0)] {
            let zero = compute_letterbox_viewport(sw, sh, fw, fh);
            assert_viewport_valid(zero, sw, sh);
            assert_eq!(zero.x, 0.0);
            assert_eq!(zero.y, 0.0);
            assert_eq!(zero.width, 0.0);
            assert_eq!(zero.height, 0.0);
        }
    }

    #[test]
    fn test_format_window_title_truncates_long_text() {
        let long_host = "example.com.".repeat(20); // 240 chars (> 200)
        assert!(long_host.len() >= 200);

        let formatted = format_window_title(&long_host, Some((1920, 1080)));
        assert!(
            formatted.ends_with(" (1920x1080)"),
            "expected resolution suffix, got: {formatted}"
        );
        assert!(
            formatted.chars().count() <= MAX_TITLE_BASE_CHARS + " (1920x1080)".len(),
            "title should be bounded, got {} chars",
            formatted.chars().count()
        );
        assert!(
            formatted.contains("... (1920x1080)"),
            "expected truncation ellipsis before suffix, got: {formatted}"
        );

        let default_cfg = ViewerWindowConfig::default();
        assert_eq!(default_cfg.title, "Perigee Viewer");
        assert_eq!(default_cfg.initial_width, 1280);
        assert_eq!(default_cfg.initial_height, 720);
        assert_eq!(
            format_window_title(&default_cfg.title, None),
            "Perigee Viewer"
        );
    }

    #[test]
    fn test_surface_recovery_requests_redraw_only_on_lost_or_outdated() {
        assert_eq!(
            classify_surface_recovery(&wgpu::CurrentSurfaceTexture::Lost),
            SurfaceRecoveryAction::ReconfigureAndRedraw
        );
        assert_eq!(
            classify_surface_recovery(&wgpu::CurrentSurfaceTexture::Outdated),
            SurfaceRecoveryAction::ReconfigureAndRedraw
        );
        assert_eq!(
            classify_surface_recovery(&wgpu::CurrentSurfaceTexture::Timeout),
            SurfaceRecoveryAction::Skip
        );
        assert_eq!(
            classify_surface_recovery(&wgpu::CurrentSurfaceTexture::Occluded),
            SurfaceRecoveryAction::Skip
        );
        assert_eq!(
            classify_surface_recovery(&wgpu::CurrentSurfaceTexture::Validation),
            SurfaceRecoveryAction::Error
        );
    }
}
