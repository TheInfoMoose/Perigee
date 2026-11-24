//! Real Wayland frame capture over `ext-image-copy-capture-v1` into a `wl_shm` buffer.
//!
//! Status: real (Phase 5a). Captures one output of the compositor this process can reach
//! (`WAYLAND_DISPLAY`, or an explicit socket path) with no consent dialog; cosmic-comp offers
//! these protocols to every unsandboxed client, the greeter included. Shared memory only: the
//! buffer is a file created and unlinked in `shm_dir` and read back with `read_exact_at`.
//! DMA-BUF and multi-output capture are Phase 5b.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};

use perigee_protocol::{MonitorInfo, PixelFormat};

use crate::backends::linux_capture::FrameCapturer;
use crate::{CaptureBackendType, CapturedFrame};

static SHM_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct CaptureCancelInner {
    cancelled: AtomicBool,
    wake_fd: OwnedFd,
}

/// Handle that cancels an in-flight or future capture on a [`WaylandShmCapturer`].
#[derive(Debug, Clone)]
pub struct CaptureCancel {
    inner: Arc<CaptureCancelInner>,
}

impl CaptureCancel {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: `eventfd` with initial value 0 and `EFD_CLOEXEC | EFD_NONBLOCK` takes no
        // pointers and returns either a new owned file descriptor or -1 with errno set.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to create eventfd for capture cancellation");
        }
        // SAFETY: `fd` was just returned as a valid, newly opened file descriptor by `eventfd`.
        let wake_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self {
            inner: Arc::new(CaptureCancelInner {
                cancelled: AtomicBool::new(false),
                wake_fd,
            }),
        })
    }

    /// Cancels the capturer and wakes any thread waiting for Wayland events.
    ///
    /// Sticky and idempotent; safe to call from any thread.
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let val: u64 = 1;
        // SAFETY: `wake_fd` is a valid open `eventfd` owned by `self.inner` for the duration of
        // this call, and `&val` points to 8 readable bytes as required by `eventfd(2)` writes.
        let _ = unsafe {
            libc::write(
                self.inner.wake_fd.as_raw_fd(),
                (&raw const val).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
    }

    /// Returns `true` once [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    fn wake_fd(&self) -> RawFd {
        self.inner.wake_fd.as_raw_fd()
    }
}

/// Waits until `fd` reports any event in `events` (`Ok(true)`) or `cancel` is triggered
/// (`Ok(false)`).
///
/// Returns `Ok(false)` immediately if `cancel` is already cancelled. Retries on `EINTR`.
pub(crate) fn wait_fd_events_or_cancelled(
    fd: BorrowedFd<'_>,
    events: libc::c_short,
    cancel: &CaptureCancel,
) -> Result<bool> {
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let mut fds = [
        libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: cancel.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        fds[0].revents = 0;
        fds[1].revents = 0;
        // SAFETY: `fds` is a valid 2-element array of `libc::pollfd` whose file descriptors
        // remain open for the duration of this call (`fd` is borrowed and `cancel` owns `wake_fd`).
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("poll failed while waiting for Wayland events");
        }
        if cancel.is_cancelled() || (fds[1].revents & libc::POLLIN) != 0 {
            return Ok(false);
        }
        if fds[0].revents != 0 {
            return Ok(true);
        }
    }
}

/// Waits until `fd` is readable (`Ok(true)`) or `cancel` is triggered (`Ok(false)`).
///
/// Returns `Ok(false)` immediately if `cancel` is already cancelled. Retries on `EINTR`.
pub(crate) fn wait_readable_or_cancelled(
    fd: BorrowedFd<'_>,
    cancel: &CaptureCancel,
) -> Result<bool> {
    wait_fd_events_or_cancelled(fd, libc::POLLIN, cancel)
}

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

#[derive(Debug, Clone)]
pub(crate) struct OutputInfo {
    pub(crate) name: String,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) refresh_mhz: i32,
    pub(crate) scale: i32,
    pub(crate) logical_x: Option<i32>,
    pub(crate) logical_y: Option<i32>,
    pub(crate) logical_width: Option<i32>,
    pub(crate) logical_height: Option<i32>,
    pub(crate) xdg_name: Option<String>,
}

impl Default for OutputInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            refresh_mhz: 0,
            scale: 1,
            logical_x: None,
            logical_y: None,
            logical_width: None,
            logical_height: None,
            xdg_name: None,
        }
    }
}

/// Converts collected `wl_output` and optional `zxdg_output_v1` metadata into a [`MonitorInfo`].
pub(crate) fn output_info_to_monitor_info(
    index: usize,
    active_output: usize,
    info: &OutputInfo,
) -> MonitorInfo {
    let name = info
        .xdg_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .or_else(|| (!info.name.is_empty()).then_some(info.name.as_str()))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("output-{index}"));
    let x = info.logical_x.unwrap_or(info.x);
    let y = info.logical_y.unwrap_or(info.y);
    let width = if info.width > 0 {
        info.width as u32
    } else {
        info.logical_width.unwrap_or(0).max(0) as u32
    };
    let height = if info.height > 0 {
        info.height as u32
    } else {
        info.logical_height.unwrap_or(0).max(0) as u32
    };
    let scale_factor = match info.logical_width {
        Some(logical_w) if logical_w > 0 && info.width > 0 => {
            (info.width as f32 / logical_w as f32).max(1.0)
        }
        _ => info.scale.max(1) as f32,
    };
    MonitorInfo {
        monitor_id: index as u32 + 1,
        name,
        x,
        y,
        width,
        height,
        scale_factor,
        refresh_rate_hz: (info.refresh_mhz.max(0) / 1000) as u32,
        is_primary: index == active_output,
    }
}

/// Formats a list of [`MonitorInfo`] entries as one human-readable line per output.
pub fn format_monitor_list(monitors: &[MonitorInfo]) -> String {
    if monitors.is_empty() {
        return "no Wayland outputs found".to_string();
    }
    monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            format!(
                "index {idx}: {} {}x{}{:+}{:+} @ {}Hz scale={:.2}",
                monitor.name,
                monitor.width,
                monitor.height,
                monitor.x,
                monitor.y,
                monitor.refresh_rate_hz,
                monitor.scale_factor
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Connects to Wayland (`socket` or `WAYLAND_DISPLAY`), enumerates all `wl_output` and optional
/// `zxdg_output_v1` metadata, and returns the resulting [`MonitorInfo`] list without creating a
/// capture session or allocating shared-memory buffers.
pub fn list_wayland_outputs(socket: Option<&Path>) -> Result<Vec<MonitorInfo>> {
    let connection = connect_wayland(socket)?;
    let (globals, mut queue) = registry_queue_init::<CaptureState>(&connection)
        .context("failed to read the Wayland registry")?;
    let mut state = CaptureState::default();
    enumerate_wayland_outputs(&globals, &mut queue, &mut state)?;
    Ok(state
        .outputs
        .iter()
        .enumerate()
        .map(|(index, (_, info))| output_info_to_monitor_info(index, 0, info))
        .collect())
}

fn connect_wayland(socket: Option<&Path>) -> Result<Connection> {
    match socket {
        Some(path) => {
            let stream = UnixStream::connect(path)
                .with_context(|| format!("failed to connect to {}", path.display()))?;
            Connection::from_socket(stream).context("failed to open the Wayland connection")
        }
        None => Connection::connect_to_env().context("failed to connect to WAYLAND_DISPLAY"),
    }
}

fn enumerate_wayland_outputs(
    globals: &wayland_client::globals::GlobalList,
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
) -> Result<()> {
    let qh = queue.handle();
    let output_globals: Vec<(u32, u32)> = globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|global| global.interface == "wl_output")
        .map(|global| (global.name, global.version))
        .collect();
    for (index, (name, version)) in output_globals.into_iter().enumerate() {
        let output: wl_output::WlOutput = globals.registry().bind(name, version.min(4), &qh, index);
        state.outputs.push((output, OutputInfo::default()));
    }
    queue.roundtrip(state).context("Wayland roundtrip failed")?;

    if let Ok(xdg_output_manager) = globals.bind::<ZxdgOutputManagerV1, _, _>(&qh, 1..=3, ()) {
        let xdg_outputs: Vec<ZxdgOutputV1> = state
            .outputs
            .iter()
            .enumerate()
            .map(|(index, (output, _))| xdg_output_manager.get_xdg_output(output, &qh, index))
            .collect();
        queue
            .roundtrip(state)
            .context("Wayland xdg-output roundtrip failed")?;
        for xdg_output in xdg_outputs {
            xdg_output.destroy();
        }
        xdg_output_manager.destroy();
    }

    Ok(())
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
            wl_output::Event::Geometry { x, y, .. } => {
                info.x = x;
                info.y = y;
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                let is_current =
                    matches!(flags, WEnum::Value(mode) if mode.contains(wl_output::Mode::Current));
                if is_current || (info.width == 0 && info.height == 0) {
                    info.width = width;
                    info.height = height;
                    info.refresh_mhz = refresh;
                }
            }
            wl_output::Event::Scale { factor } => {
                info.scale = factor;
            }
            wl_output::Event::Name { name } => info.name = name,
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, info)) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                info.logical_x = Some(x);
                info.logical_y = Some(y);
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                info.logical_width = Some(width);
                info.logical_height = Some(height);
            }
            zxdg_output_v1::Event::Name { name } => {
                info.xdg_name = Some(name);
            }
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
delegate_noop!(CaptureState: ZxdgOutputManagerV1);

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
    cancel: CaptureCancel,
}

impl WaylandShmCapturer {
    /// Connects to `socket` (or `WAYLAND_DISPLAY` when `None`), opens a capture session for
    /// output `output_index` (0 = the first `wl_output` the compositor lists), and allocates its
    /// shared-memory buffer in `shm_dir` (the session's `XDG_RUNTIME_DIR`).
    pub fn connect(socket: Option<&Path>, shm_dir: &Path, output_index: usize) -> Result<Self> {
        let connection = connect_wayland(socket)?;
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
        enumerate_wayland_outputs(&globals, &mut queue, &mut state)?;
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
        let cancel = CaptureCancel::new()?;
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
            cancel,
        })
    }

    /// Returns a handle that cancels an in-flight or future capture on this capturer.
    pub fn canceller(&self) -> CaptureCancel {
        self.cancel.clone()
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
        if self.cancel.is_cancelled() {
            bail!("capture cancelled");
        }
        let queue = self
            .queue
            .get_mut()
            .map_err(|_| anyhow!("Wayland queue lock poisoned"))?;
        self.state.frame_result = None;
        let frame = self.session.create_frame(&self.qh, ());
        frame.attach_buffer(&self.buffer.buffer);
        frame.damage_buffer(0, 0, self.buffer.width as i32, self.buffer.height as i32);
        frame.capture();
        let mut cancelled = false;
        while self.state.frame_result.is_none() && !self.state.stopped {
            if !dispatch_or_cancelled(queue, &mut self.state, &self.cancel)
                .context("Wayland dispatch failed while capturing")?
            {
                cancelled = true;
                break;
            }
        }
        frame.destroy();
        if cancelled {
            if let Err(err) = queue.flush() {
                tracing::debug!(error = %err, "Wayland flush after cancelling capture failed");
            }
            bail!("capture cancelled");
        }
        if self.state.stopped {
            bail!("the compositor stopped the capture session");
        }
        self.state
            .frame_result
            .take()
            .ok_or_else(|| anyhow!("capture ended without a result"))
    }
}

fn dispatch_or_cancelled(
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
    cancel: &CaptureCancel,
) -> Result<bool> {
    if cancel.is_cancelled() {
        return Ok(false);
    }
    if queue
        .dispatch_pending(state)
        .context("Wayland dispatch_pending failed")?
        > 0
    {
        return Ok(true);
    }
    loop {
        match queue.flush() {
            Ok(()) => break,
            Err(wayland_client::backend::WaylandError::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock =>
            {
                if !wait_fd_events_or_cancelled(queue.as_fd(), libc::POLLOUT, cancel)? {
                    return Ok(false);
                }
            }
            Err(err) => return Err(err).context("Wayland flush failed"),
        }
    }
    let Some(guard) = queue.prepare_read() else {
        queue
            .dispatch_pending(state)
            .context("Wayland dispatch_pending failed")?;
        return Ok(true);
    };
    let readable = wait_readable_or_cancelled(guard.connection_fd(), cancel)?;
    if !readable {
        drop(guard);
        return Ok(false);
    }
    match guard.read() {
        Ok(_) => {}
        Err(wayland_client::backend::WaylandError::Io(err))
            if err.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(err) => return Err(err).context("Wayland read failed"),
    }
    queue
        .dispatch_pending(state)
        .context("Wayland dispatch_pending failed")?;
    Ok(true)
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

fn wait_for_constraints_or_cancelled(
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
    cancel: &CaptureCancel,
) -> Result<()> {
    while !state.constraints_done {
        if state.stopped {
            bail!("the compositor stopped the capture session before offering a buffer");
        }
        if !dispatch_or_cancelled(queue, state, cancel)
            .context("Wayland dispatch failed while waiting for buffer constraints")?
        {
            bail!("capture cancelled");
        }
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
            .map(|(index, (_, info))| output_info_to_monitor_info(index, self.output_index, info))
            .collect())
    }

    fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
        if self.cancel.is_cancelled() {
            bail!("capture cancelled");
        }
        let mut result = self.capture_once()?;
        if let FrameResult::Failed(reason) = &result
            && reason.contains("BufferConstraints")
        {
            // The output changed size: wait for the new constraints, reallocate, retry once.
            let queue = self
                .queue
                .get_mut()
                .map_err(|_| anyhow!("Wayland queue lock poisoned"))?;
            wait_for_constraints_or_cancelled(queue, &mut self.state, &self.cancel)?;
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

    #[test]
    fn test_wait_returns_false_when_cancelled() {
        let (rx, _tx) = UnixStream::pair().expect("UnixStream::pair should succeed");
        let cancel = CaptureCancel::new().expect("CaptureCancel::new should succeed");
        let cancel_worker = cancel.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let res = wait_readable_or_cancelled(rx.as_fd(), &cancel_worker);
            (res, started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.cancel();
        let (res, elapsed) = handle.join().expect("waiter thread should not panic");
        assert!(
            !res.expect("wait should return Ok(false)"),
            "expected Ok(false)"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "wait took too long after cancel: {elapsed:?}"
        );
    }

    #[test]
    fn test_wait_returns_true_when_readable() {
        use std::io::Write;
        let (rx, mut tx) = UnixStream::pair().expect("UnixStream::pair should succeed");
        let cancel = CaptureCancel::new().expect("CaptureCancel::new should succeed");
        tx.write_all(&[1u8])
            .expect("writing one byte should succeed");
        let readable =
            wait_readable_or_cancelled(rx.as_fd(), &cancel).expect("wait should succeed");
        assert!(readable, "expected Ok(true) when peer wrote a byte");
    }

    #[test]
    fn test_wait_after_cancel_returns_immediately() {
        let (rx, _tx) = UnixStream::pair().expect("UnixStream::pair should succeed");
        let cancel = CaptureCancel::new().expect("CaptureCancel::new should succeed");
        cancel.cancel();
        assert!(cancel.is_cancelled());
        let started = Instant::now();
        let readable =
            wait_readable_or_cancelled(rx.as_fd(), &cancel).expect("wait should succeed");
        assert!(!readable, "expected Ok(false) after cancel");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "wait after cancel took too long: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn test_wait_fd_events_or_cancelled_pollout_and_cancel() {
        use std::io::{Read, Write};
        let (mut rx, mut tx) = UnixStream::pair().expect("UnixStream::pair should succeed");
        tx.set_nonblocking(true)
            .expect("set_nonblocking on tx should succeed");
        rx.set_nonblocking(true)
            .expect("set_nonblocking on rx should succeed");

        let chunk = [0xABu8; 8192];
        loop {
            match tx.write(&chunk) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => panic!("unexpected write error while filling tx buffer: {err}"),
            }
        }
        loop {
            match tx.write(&[0xABu8; 1]) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => panic!("unexpected write error while topping off tx buffer: {err}"),
            }
        }

        let cancel = CaptureCancel::new().expect("CaptureCancel::new should succeed");
        let cancel_worker = cancel.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let res = wait_fd_events_or_cancelled(tx.as_fd(), libc::POLLOUT, &cancel_worker);
            (tx, res, started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.cancel();
        let (tx, res, elapsed) = handle.join().expect("waiter thread should not panic");
        assert!(
            !res.expect("wait should return Ok(false) on cancel"),
            "expected Ok(false) when cancelled while waiting for POLLOUT"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "POLLOUT wait took too long after cancel: {elapsed:?}"
        );

        let cancel2 = CaptureCancel::new().expect("CaptureCancel::new should succeed");
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let res = wait_fd_events_or_cancelled(tx.as_fd(), libc::POLLOUT, &cancel2);
            (res, started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut drain = [0u8; 65536];
        loop {
            match rx.read(&mut drain) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => panic!("unexpected read error while draining rx: {err}"),
            }
        }
        let (res, elapsed) = handle.join().expect("waiter thread should not panic");
        assert!(
            res.expect("wait should return Ok(true) once peer drains rx"),
            "expected Ok(true) once peer reads from rx"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "POLLOUT wait took too long after peer read: {elapsed:?}"
        );
    }

    #[test]
    fn test_output_info_prefers_xdg_logical_geometry_and_computes_fractional_scale() {
        let xdg_info = OutputInfo {
            name: "wl-DP-1".to_string(),
            x: 100,
            y: 200,
            width: 3840,
            height: 2160,
            refresh_mhz: 60_000,
            scale: 2,
            logical_x: Some(1920),
            logical_y: Some(0),
            logical_width: Some(2560),
            logical_height: Some(1440),
            xdg_name: Some("DP-1".to_string()),
        };
        let mon = output_info_to_monitor_info(1, 0, &xdg_info);
        assert_eq!(mon.monitor_id, 2);
        assert_eq!(mon.name, "DP-1");
        assert_eq!(mon.x, 1920);
        assert_eq!(mon.y, 0);
        assert_eq!(mon.width, 3840);
        assert_eq!(mon.height, 2160);
        assert_eq!(mon.refresh_rate_hz, 60);
        assert!(!mon.is_primary);
        assert!(
            (mon.scale_factor - 1.5).abs() < 0.01,
            "expected scale_factor ~1.5, got {}",
            mon.scale_factor
        );

        // Fallback when xdg_output fields are None uses wl_output x/y/scale and name.
        let wl_only = OutputInfo {
            name: "HDMI-A-1".to_string(),
            x: 0,
            y: 1080,
            width: 1920,
            height: 1080,
            refresh_mhz: 144_000,
            scale: 2,
            logical_x: None,
            logical_y: None,
            logical_width: None,
            logical_height: None,
            xdg_name: None,
        };
        let fallback = output_info_to_monitor_info(0, 0, &wl_only);
        assert_eq!(fallback.monitor_id, 1);
        assert_eq!(fallback.name, "HDMI-A-1");
        assert_eq!(fallback.x, 0);
        assert_eq!(fallback.y, 1080);
        assert_eq!(fallback.width, 1920);
        assert_eq!(fallback.height, 1080);
        assert_eq!(fallback.refresh_rate_hz, 144);
        assert!(fallback.is_primary);
        assert!(
            (fallback.scale_factor - 2.0).abs() < 0.01,
            "expected scale_factor 2.0, got {}",
            fallback.scale_factor
        );

        // Empty name falls back to output-<index>.
        let unnamed = OutputInfo::default();
        let default_mon = output_info_to_monitor_info(2, 0, &unnamed);
        assert_eq!(default_mon.name, "output-2");
        assert!((default_mon.scale_factor - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_format_monitor_list_formats_multiple_outputs_and_empty() {
        assert_eq!(format_monitor_list(&[]), "no Wayland outputs found");

        let monitors = vec![
            MonitorInfo {
                monitor_id: 1,
                name: "eDP-1".to_string(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale_factor: 1.0,
                refresh_rate_hz: 60,
                is_primary: true,
            },
            MonitorInfo {
                monitor_id: 2,
                name: "DP-1".to_string(),
                x: 1920,
                y: 0,
                width: 3840,
                height: 2160,
                scale_factor: 1.5,
                refresh_rate_hz: 120,
                is_primary: false,
            },
        ];
        let formatted = format_monitor_list(&monitors);
        let lines: Vec<&str> = formatted.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "index 0: eDP-1 1920x1080+0+0 @ 60Hz scale=1.00");
        assert_eq!(
            lines[1],
            "index 1: DP-1 3840x2160+1920+0 @ 120Hz scale=1.50"
        );
    }
}
