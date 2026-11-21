//! Real Wayland frame capture over `ext-image-copy-capture-v1` into a `wl_shm` buffer.
//!
//! Status: real (Phase 5a). Captures one output of the compositor this process can reach
//! (`WAYLAND_DISPLAY`, or an explicit socket path) with no consent dialog; cosmic-comp offers
//! these protocols to every unsandboxed client, the greeter included. Shared memory only: the
//! buffer is a file created and unlinked in `shm_dir` and read back with `read_exact_at`.
//! DMA-BUF and multi-output capture are Phase 5b.

use std::fs::{File, OpenOptions};
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use tracing::info;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use perigee_protocol::{MonitorInfo, PixelFormat};

use crate::backends::linux_capture::FrameCapturer;
use crate::{CaptureBackendType, CapturedFrame};

static SHM_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Picks the shared-memory format to capture in, preferring formats whose memory layout is
/// `Bgra8` (`Xrgb8888`, `Argb8888`) over `Rgba8` (`Xbgr8888`, `Abgr8888`).
pub fn pick_shm_format(offered: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    [
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xbgr8888,
        wl_shm::Format::Abgr8888,
    ]
    .into_iter()
    .find(|wanted| offered.contains(wanted))
}

/// Maps a `wl_shm` format to the wire [`PixelFormat`] of its bytes in memory.
pub fn shm_format_to_pixel_format(format: wl_shm::Format) -> Option<PixelFormat> {
    match format {
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(PixelFormat::Bgra8),
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(PixelFormat::Rgba8),
        _ => None,
    }
}

#[derive(Debug, Clone, Default)]
struct OutputInfo {
    name: String,
    width: i32,
    height: i32,
    refresh_mhz: i32,
}

enum FrameResult {
    Ready,
    Failed(String),
}

#[derive(Default)]
struct CaptureState {
    outputs: Vec<(wl_output::WlOutput, OutputInfo)>,
    buffer_width: u32,
    buffer_height: u32,
    shm_formats: Vec<wl_shm::Format>,
    constraints_done: bool,
    stopped: bool,
    frame_result: Option<FrameResult>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for CaptureState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, usize> for CaptureState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, info)) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Mode {
                width,
                height,
                refresh,
                ..
            } => {
                info.width = width;
                info.height = height;
                info.refresh_mhz = refresh;
            }
            wl_output::Event::Name { name } => info.name = name,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.buffer_width = width;
                state.buffer_height = height;
                state.shm_formats.clear();
                state.constraints_done = false;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } => state.shm_formats.push(format),
            ext_image_copy_capture_session_v1::Event::Done => state.constraints_done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => state.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                state.frame_result = Some(FrameResult::Ready);
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                state.frame_result = Some(FrameResult::Failed(format!("{reason:?}")));
            }
            _ => {}
        }
    }
}

delegate_noop!(CaptureState: ignore wl_shm::WlShm);
delegate_noop!(CaptureState: ignore wl_buffer::WlBuffer);
delegate_noop!(CaptureState: wl_shm_pool::WlShmPool);
delegate_noop!(CaptureState: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(CaptureState: ExtImageCaptureSourceV1);
delegate_noop!(CaptureState: ExtImageCopyCaptureManagerV1);

/// One allocated `wl_shm` buffer matching the session's current constraints.
struct ShmBuffer {
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: PixelFormat,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

/// Real [`FrameCapturer`] for one Wayland output through `ext-image-copy-capture-v1`.
pub struct WaylandShmCapturer {
    _connection: Connection,
    queue: Mutex<EventQueue<CaptureState>>,
    qh: QueueHandle<CaptureState>,
    state: CaptureState,
    shm: wl_shm::WlShm,
    session: ExtImageCopyCaptureSessionV1,
    _source: ExtImageCaptureSourceV1,
    buffer: ShmBuffer,
    shm_dir: std::path::PathBuf,
    output_index: usize,
    frame_counter: u64,
    start: Instant,
}

impl WaylandShmCapturer {
    /// Connects to `socket` (or `WAYLAND_DISPLAY` when `None`), opens a capture session for
    /// output `output_index` (0 = the first `wl_output` the compositor lists), and allocates its
    /// shared-memory buffer in `shm_dir` (the session's `XDG_RUNTIME_DIR`).
    pub fn connect(socket: Option<&Path>, shm_dir: &Path, output_index: usize) -> Result<Self> {
        let connection = match socket {
            Some(path) => {
                let stream = UnixStream::connect(path)
                    .with_context(|| format!("failed to connect to {}", path.display()))?;
                Connection::from_socket(stream).context("failed to open the Wayland connection")?
            }
            None => Connection::connect_to_env().context("failed to connect to WAYLAND_DISPLAY")?,
        };
        let (globals, mut queue) = registry_queue_init::<CaptureState>(&connection)
            .context("failed to read the Wayland registry")?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals
            .bind(&qh, 1..=1, ())
            .context("compositor has no wl_shm")?;
        let source_manager: ExtOutputImageCaptureSourceManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor has no ext_output_image_capture_source_manager_v1")?;
        let copy_manager: ExtImageCopyCaptureManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor has no ext_image_copy_capture_manager_v1")?;

        let mut state = CaptureState::default();
        let output_globals: Vec<(u32, u32)> = globals
            .contents()
            .clone_list()
            .into_iter()
            .filter(|global| global.interface == "wl_output")
            .map(|global| (global.name, global.version))
            .collect();
        for (index, (name, version)) in output_globals.into_iter().enumerate() {
            let output: wl_output::WlOutput =
                globals.registry().bind(name, version.min(4), &qh, index);
            state.outputs.push((output, OutputInfo::default()));
        }
        queue
            .roundtrip(&mut state)
            .context("Wayland roundtrip failed")?;
        let output = state
            .outputs
            .get(output_index)
            .map(|(output, _)| output.clone())
            .ok_or_else(|| {
                anyhow!(
                    "output {output_index} does not exist; the compositor lists {} output(s)",
                    state.outputs.len()
                )
            })?;

        let source = source_manager.create_source(&output, &qh, ());
        let session = copy_manager.create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::PaintCursors,
            &qh,
            (),
        );
        wait_for_constraints(&mut queue, &mut state)?;
        let buffer = allocate_buffer(&shm, &qh, &state, shm_dir)?;
        info!(
            output_index,
            width = buffer.width,
            height = buffer.height,
            format = ?buffer.pixel_format,
            "Started ext-image-copy-capture-v1 shared-memory capture"
        );
        Ok(Self {
            _connection: connection,
            queue: Mutex::new(queue),
            qh,
            state,
            shm,
            session,
            _source: source,
            buffer,
            shm_dir: shm_dir.to_path_buf(),
            output_index,
            frame_counter: 0,
            start: Instant::now(),
        })
    }

    /// Width in pixels of the frames this capturer returns.
    pub fn width(&self) -> u32 {
        self.buffer.width
    }

    /// Height in pixels of the frames this capturer returns.
    pub fn height(&self) -> u32 {
        self.buffer.height
    }

    fn capture_once(&mut self) -> Result<FrameResult> {
        let queue = self
            .queue
            .get_mut()
            .map_err(|_| anyhow!("Wayland queue lock poisoned"))?;
        self.state.frame_result = None;
        let frame = self.session.create_frame(&self.qh, ());
        frame.attach_buffer(&self.buffer.buffer);
        frame.damage_buffer(0, 0, self.buffer.width as i32, self.buffer.height as i32);
        frame.capture();
        while self.state.frame_result.is_none() && !self.state.stopped {
            queue
                .blocking_dispatch(&mut self.state)
                .context("Wayland dispatch failed while capturing")?;
        }
        frame.destroy();
        if self.state.stopped {
            bail!("the compositor stopped the capture session");
        }
        self.state
            .frame_result
            .take()
            .ok_or_else(|| anyhow!("capture ended without a result"))
    }
}

fn wait_for_constraints(
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
) -> Result<()> {
    while !state.constraints_done {
        if state.stopped {
            bail!("the compositor stopped the capture session before offering a buffer");
        }
        queue
            .blocking_dispatch(state)
            .context("Wayland dispatch failed while waiting for buffer constraints")?;
    }
    Ok(())
}

fn allocate_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<CaptureState>,
    state: &CaptureState,
    shm_dir: &Path,
) -> Result<ShmBuffer> {
    let format = pick_shm_format(&state.shm_formats).ok_or_else(|| {
        anyhow!(
            "compositor offers no 32-bit RGB shm format (offered {:?})",
            state.shm_formats
        )
    })?;
    let pixel_format = shm_format_to_pixel_format(format)
        .ok_or_else(|| anyhow!("unsupported shm format {format:?}"))?;
    let (width, height) = (state.buffer_width, state.buffer_height);
    if width == 0 || height == 0 {
        bail!("compositor offered an empty {width}x{height} buffer");
    }
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| anyhow!("buffer width {width} overflows"))?;
    let size = u64::from(stride) * u64::from(height);
    let size_i32 =
        i32::try_from(size).map_err(|_| anyhow!("{width}x{height} buffer is too large"))?;
    let path = shm_dir.join(format!(
        ".perigee-capture-{}-{}",
        std::process::id(),
        SHM_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    std::fs::remove_file(&path).with_context(|| format!("failed to unlink {}", path.display()))?;
    file.set_len(size)
        .context("failed to size the shared-memory buffer")?;
    let pool = shm.create_pool(file.as_fd(), size_i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        format,
        qh,
        (),
    );
    Ok(ShmBuffer {
        file,
        pool,
        buffer,
        width,
        height,
        stride,
        pixel_format,
    })
}

impl FrameCapturer for WaylandShmCapturer {
    fn backend_type(&self) -> CaptureBackendType {
        CaptureBackendType::WaylandPipeWire
    }

    fn is_live_display(&self) -> bool {
        !self.state.stopped
    }

    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
        Ok(self
            .state
            .outputs
            .iter()
            .enumerate()
            .map(|(index, (_, info))| MonitorInfo {
                monitor_id: index as u32 + 1,
                name: info.name.clone(),
                x: 0,
                y: 0,
                width: info.width.max(0) as u32,
                height: info.height.max(0) as u32,
                scale_factor: 1.0,
                refresh_rate_hz: (info.refresh_mhz.max(0) / 1000) as u32,
                is_primary: index == self.output_index,
            })
            .collect())
    }

    fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
        let mut result = self.capture_once()?;
        if let FrameResult::Failed(reason) = &result
            && reason.contains("BufferConstraints")
        {
            // The output changed size: wait for the new constraints, reallocate, retry once.
            let queue = self
                .queue
                .get_mut()
                .map_err(|_| anyhow!("Wayland queue lock poisoned"))?;
            wait_for_constraints(queue, &mut self.state)?;
            self.buffer = allocate_buffer(&self.shm, &self.qh, &self.state, &self.shm_dir)?;
            result = self.capture_once()?;
        }
        if let FrameResult::Failed(reason) = result {
            bail!("the compositor failed the capture: {reason}");
        }
        let mut data = vec![0u8; self.buffer.stride as usize * self.buffer.height as usize];
        self.buffer
            .file
            .read_exact_at(&mut data, 0)
            .context("failed to read the captured frame")?;
        self.frame_counter += 1;
        Ok(CapturedFrame {
            frame_index: self.frame_counter,
            timestamp_us: self.start.elapsed().as_micros() as u64,
            display_id: self.output_index as u32 + 1,
            width: self.buffer.width,
            height: self.buffer.height,
            stride: self.buffer.stride,
            pixel_format: self.buffer.pixel_format,
            data,
            is_dmabuf_or_metal_handle: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pick_shm_format_prefers_bgra_layouts() {
        assert_eq!(
            pick_shm_format(&[wl_shm::Format::Abgr8888, wl_shm::Format::Argb8888]),
            Some(wl_shm::Format::Argb8888)
        );
        assert_eq!(
            pick_shm_format(&[wl_shm::Format::Xbgr8888]),
            Some(wl_shm::Format::Xbgr8888)
        );
        assert_eq!(pick_shm_format(&[wl_shm::Format::Rgb565]), None);
        assert_eq!(pick_shm_format(&[]), None);
    }

    #[test]
    fn test_shm_format_to_pixel_format() {
        assert_eq!(
            shm_format_to_pixel_format(wl_shm::Format::Xrgb8888),
            Some(PixelFormat::Bgra8)
        );
        assert_eq!(
            shm_format_to_pixel_format(wl_shm::Format::Abgr8888),
            Some(PixelFormat::Rgba8)
        );
        assert_eq!(shm_format_to_pixel_format(wl_shm::Format::Rgb565), None);
    }

    #[test]
    fn test_connect_to_missing_socket_fails_cleanly() {
        let dir = std::env::temp_dir();
        let missing = dir.join(format!("perigee-no-wayland-{}", std::process::id()));
        let err = WaylandShmCapturer::connect(Some(&missing), &dir, 0)
            .err()
            .expect("connecting to a missing socket must fail");
        assert!(
            format!("{err:#}").contains("failed to connect to"),
            "got: {err:#}"
        );
    }
}
