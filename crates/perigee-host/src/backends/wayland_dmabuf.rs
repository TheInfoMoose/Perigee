//! Real Wayland DMA-BUF frame capture over `ext-image-copy-capture-v1`,
//! `zwp_linux_dmabuf_v1`, and `gbm`, with automatic `wl_shm` fallback.

use anyhow::{Result, bail};

/// Capture transport mode for Linux Wayland capture (`--capture-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    /// Prefer DMA-BUF (`ext-image-copy-capture-v1` + `zwp_linux_dmabuf_v1` + `gbm`)
    /// and automatically fall back to `wl_shm` if DMA-BUF is unavailable.
    #[default]
    Auto,
    /// Require DMA-BUF capture (fails closed if unavailable).
    Dmabuf,
    /// Use `wl_shm` shared-memory capture directly.
    Shm,
}

/// Parses a `--capture-mode` value (`auto`, `dmabuf`, or `shm`, case-insensitive).
pub fn parse_capture_mode(value: &str) -> Result<CaptureMode> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Ok(CaptureMode::Auto),
        "dmabuf" => Ok(CaptureMode::Dmabuf),
        "shm" => Ok(CaptureMode::Shm),
        other => bail!("unknown capture mode '{other}': expected auto, dmabuf, or shm"),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsFd;
    use std::os::linux::fs::MetadataExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Instant;

    use anyhow::{Context, Result, anyhow, bail};
    use gbm::{
        BufferObject, BufferObjectFlags, Device as GbmDevice, Format as GbmFormat, Modifier,
    };
    use tracing::{info, warn};
    use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_buffer, wl_output, wl_registry};
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
    use wayland_protocols::wp::linux_dmabuf::zv1::client::{
        zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    };
    use wayland_protocols::xdg::xdg_output::zv1::client::{
        zxdg_output_manager_v1::ZxdgOutputManagerV1,
        zxdg_output_v1::{self, ZxdgOutputV1},
    };

    use perigee_protocol::{MonitorInfo, PixelFormat};

    use super::CaptureMode;
    use crate::backends::linux_capture::{
        DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_XBGR8888, DRM_FORMAT_XRGB8888,
        FrameCapturer,
    };
    use crate::backends::wayland_shm::{
        CaptureCancel, OutputInfo, WaylandShmCapturer, output_info_to_monitor_info,
        wait_fd_events_or_cancelled, wait_readable_or_cancelled,
    };
    use crate::{CaptureBackendType, CapturedFrame};

    /// Linear DRM format modifier (`DRM_FORMAT_MOD_LINEAR`).
    pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

    /// Invalid/implicit DRM format modifier (`DRM_FORMAT_MOD_INVALID`).
    pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

    const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";
    const DRI_DIR: &str = "/dev/dri";

    /// Selects a 32-bit RGB DRM fourcc and modifier from the session's offered DMA-BUF formats.
    ///
    /// Prefers `Bgra8` layouts (`XRGB8888`, `ARGB8888`) over `Rgba8` layouts (`XBGR8888`,
    /// `ABGR8888`), and prefers linear modifier `0` (`DRM_FORMAT_MOD_LINEAR`) when offered for
    /// that format; otherwise falls back to the first offered modifier or
    /// `0x00ff_ffff_ffff_ffff` (`DRM_FORMAT_MOD_INVALID`).
    pub fn pick_dmabuf_format(offered: &[(u32, Vec<u64>)]) -> Option<(u32, PixelFormat, u64)> {
        const PREFERRED: &[(u32, PixelFormat)] = &[
            (DRM_FORMAT_XRGB8888, PixelFormat::Bgra8),
            (DRM_FORMAT_ARGB8888, PixelFormat::Bgra8),
            (DRM_FORMAT_XBGR8888, PixelFormat::Rgba8),
            (DRM_FORMAT_ABGR8888, PixelFormat::Rgba8),
        ];

        for &(fourcc, pixel_format) in PREFERRED {
            let mut matched = false;
            let mut has_linear = false;
            let mut first_mod = None;
            for (offered_fourcc, mods) in offered {
                if *offered_fourcc != fourcc {
                    continue;
                }
                matched = true;
                if mods.contains(&DRM_FORMAT_MOD_LINEAR) {
                    has_linear = true;
                }
                if first_mod.is_none() {
                    first_mod = mods.first().copied();
                }
            }
            if matched {
                let modifier = if has_linear {
                    DRM_FORMAT_MOD_LINEAR
                } else {
                    first_mod.unwrap_or(DRM_FORMAT_MOD_INVALID)
                };
                return Some((fourcc, pixel_format, modifier));
            }
        }
        None
    }

    /// Decodes a native-endian `dev_t` from `ext_image_copy_capture_session_v1::Event::DmabufDevice`
    /// and returns the matching DRM device path in `dri_dir`, preferring `renderD*` nodes when
    /// multiple entries share the same `st_rdev()`.
    pub fn resolve_render_node_from_dev_t(dev_bytes: &[u8], dri_dir: &Path) -> Option<PathBuf> {
        let target_dev = match dev_bytes.len() {
            8 => {
                let arr: [u8; 8] = dev_bytes.try_into().ok()?;
                u64::from_ne_bytes(arr)
            }
            4 => {
                let arr: [u8; 4] = dev_bytes.try_into().ok()?;
                u64::from(u32::from_ne_bytes(arr))
            }
            _ => return None,
        };

        let entries = std::fs::read_dir(dri_dir).ok()?;
        let mut fallback: Option<PathBuf> = None;
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.st_rdev() != target_dev {
                continue;
            }
            let path = entry.path();
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("renderD"))
            {
                return Some(path);
            }
            if fallback.is_none() {
                fallback = Some(path);
            }
        }
        fallback
    }

    enum FrameResult {
        Ready,
        Failed(String),
    }

    #[derive(Default)]
    struct DmabufCaptureState {
        outputs: Vec<(wl_output::WlOutput, OutputInfo)>,
        buffer_width: u32,
        buffer_height: u32,
        dmabuf_device: Option<Vec<u8>>,
        dmabuf_formats: Vec<(u32, Vec<u64>)>,
        constraints_done: bool,
        stopped: bool,
        frame_result: Option<FrameResult>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for DmabufCaptureState {
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

    impl Dispatch<wl_output::WlOutput, usize> for DmabufCaptureState {
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
                    let is_current = matches!(
                        flags,
                        WEnum::Value(mode) if mode.contains(wl_output::Mode::Current)
                    );
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

    impl Dispatch<ZxdgOutputV1, usize> for DmabufCaptureState {
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

    impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for DmabufCaptureState {
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
                    state.dmabuf_formats.clear();
                    state.constraints_done = false;
                }
                ext_image_copy_capture_session_v1::Event::DmabufDevice { device } => {
                    state.dmabuf_device = Some(device);
                }
                ext_image_copy_capture_session_v1::Event::DmabufFormat { format, modifiers } => {
                    let parsed_mods: Vec<u64> = modifiers
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .copied()
                        .map(u64::from_ne_bytes)
                        .collect();
                    state.dmabuf_formats.push((format, parsed_mods));
                }
                ext_image_copy_capture_session_v1::Event::Done => state.constraints_done = true,
                ext_image_copy_capture_session_v1::Event::Stopped => state.stopped = true,
                _ => {}
            }
        }
    }

    impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for DmabufCaptureState {
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

    delegate_noop!(DmabufCaptureState: ignore ZwpLinuxDmabufV1);
    delegate_noop!(DmabufCaptureState: ignore ZwpLinuxBufferParamsV1);
    delegate_noop!(DmabufCaptureState: ignore wl_buffer::WlBuffer);
    delegate_noop!(DmabufCaptureState: ExtOutputImageCaptureSourceManagerV1);
    delegate_noop!(DmabufCaptureState: ExtImageCaptureSourceV1);
    delegate_noop!(DmabufCaptureState: ExtImageCopyCaptureManagerV1);
    delegate_noop!(DmabufCaptureState: ZxdgOutputManagerV1);

    /// One allocated GBM DMA-BUF buffer imported as a `wl_buffer`.
    struct DmabufBuffer {
        wl_buffer: wl_buffer::WlBuffer,
        bo: BufferObject<()>,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    }

    impl Drop for DmabufBuffer {
        fn drop(&mut self) {
            self.wl_buffer.destroy();
        }
    }

    /// Real [`FrameCapturer`] for one Wayland output using `ext-image-copy-capture-v1`,
    /// `zwp_linux_dmabuf_v1`, and `gbm`.
    pub struct ExtDmabufCapturer {
        _connection: Connection,
        queue: Mutex<EventQueue<DmabufCaptureState>>,
        qh: QueueHandle<DmabufCaptureState>,
        state: DmabufCaptureState,
        dmabuf: ZwpLinuxDmabufV1,
        gbm: GbmDevice<File>,
        session: ExtImageCopyCaptureSessionV1,
        _source: ExtImageCaptureSourceV1,
        buffer: DmabufBuffer,
        output_index: usize,
        frame_counter: u64,
        start: Instant,
        cancel: CaptureCancel,
    }

    impl ExtDmabufCapturer {
        /// Connects to `socket` (or `WAYLAND_DISPLAY` when `None`), opens a DMA-BUF capture
        /// session for `output_index`, allocates a GBM buffer on the resolved DRM render node,
        /// and imports it via `zwp_linux_dmabuf_v1`.
        pub fn connect(socket: Option<&Path>, output_index: usize) -> Result<Self> {
            let connection = connect_wayland(socket)?;
            let (globals, mut queue) = registry_queue_init::<DmabufCaptureState>(&connection)
                .context("failed to read the Wayland registry")?;
            let qh = queue.handle();
            let dmabuf: ZwpLinuxDmabufV1 = globals
                .bind(&qh, 2..=4, ())
                .context("compositor has no zwp_linux_dmabuf_v1 (v2+)")?;
            let source_manager: ExtOutputImageCaptureSourceManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .context("compositor has no ext_output_image_capture_source_manager_v1")?;
            let copy_manager: ExtImageCopyCaptureManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .context("compositor has no ext_image_copy_capture_manager_v1")?;

            let mut state = DmabufCaptureState::default();
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

            let render_node = state
                .dmabuf_device
                .as_deref()
                .and_then(|bytes| resolve_render_node_from_dev_t(bytes, Path::new(DRI_DIR)))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_RENDER_NODE));
            let drm_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&render_node)
                .with_context(|| {
                    format!("failed to open DRM render node {}", render_node.display())
                })?;
            let gbm = GbmDevice::new(drm_file).with_context(|| {
                format!(
                    "failed to initialize GBM device on {}",
                    render_node.display()
                )
            })?;

            let buffer = allocate_dmabuf_buffer(&gbm, &dmabuf, &qh, &state)?;
            let cancel = CaptureCancel::new()?;
            info!(
                output_index,
                render_node = %render_node.display(),
                width = buffer.width,
                height = buffer.height,
                format = ?buffer.pixel_format,
                "Started ext-image-copy-capture-v1 DMA-BUF capture"
            );
            Ok(Self {
                _connection: connection,
                queue: Mutex::new(queue),
                qh,
                state,
                dmabuf,
                gbm,
                session,
                _source: source,
                buffer,
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
            frame.attach_buffer(&self.buffer.wl_buffer);
            frame.damage_buffer(0, 0, self.buffer.width as i32, self.buffer.height as i32);
            frame.capture();
            let mut cancelled = false;
            while self.state.frame_result.is_none() && !self.state.stopped {
                if !dispatch_or_cancelled(queue, &mut self.state, &self.cancel)
                    .context("Wayland dispatch failed while capturing DMA-BUF frame")?
                {
                    cancelled = true;
                    break;
                }
            }
            frame.destroy();
            if cancelled {
                if let Err(err) = queue.flush() {
                    tracing::debug!(
                        error = %err,
                        "Wayland flush after cancelling DMA-BUF capture failed"
                    );
                }
                bail!("capture cancelled");
            }
            if self.state.stopped {
                bail!("the compositor stopped the DMA-BUF capture session");
            }
            self.state
                .frame_result
                .take()
                .ok_or_else(|| anyhow!("DMA-BUF capture ended without a result"))
        }
    }

    impl FrameCapturer for ExtDmabufCapturer {
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
                .map(|(index, (_, info))| {
                    output_info_to_monitor_info(index, self.output_index, info)
                })
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
                let queue = self
                    .queue
                    .get_mut()
                    .map_err(|_| anyhow!("Wayland queue lock poisoned"))?;
                wait_for_constraints_or_cancelled(queue, &mut self.state, &self.cancel)?;
                self.buffer =
                    allocate_dmabuf_buffer(&self.gbm, &self.dmabuf, &self.qh, &self.state)?;
                result = self.capture_once()?;
            }
            if let FrameResult::Failed(reason) = result {
                bail!("the compositor failed the DMA-BUF capture: {reason}");
            }

            let (stride, data) = self
                .buffer
                .bo
                .map(0, 0, self.buffer.width, self.buffer.height, |mapped| {
                    (mapped.stride(), mapped.buffer().to_vec())
                })
                .context("failed to map GBM buffer object for CPU readback")?;

            self.frame_counter += 1;
            Ok(CapturedFrame {
                frame_index: self.frame_counter,
                timestamp_us: self.start.elapsed().as_micros() as u64,
                display_id: self.output_index as u32 + 1,
                width: self.buffer.width,
                height: self.buffer.height,
                stride,
                pixel_format: self.buffer.pixel_format,
                data,
                is_dmabuf_or_metal_handle: true,
            })
        }
    }

    /// Wayland output capturer that selects either DMA-BUF or `wl_shm` based on [`CaptureMode`].
    pub enum WaylandCapturer {
        Dmabuf(ExtDmabufCapturer),
        Shm(WaylandShmCapturer),
    }

    impl WaylandCapturer {
        /// Connects to Wayland using `mode`:
        /// - [`CaptureMode::Shm`]: uses [`WaylandShmCapturer`] directly.
        /// - [`CaptureMode::Dmabuf`]: uses [`ExtDmabufCapturer`] (fails closed if unavailable).
        /// - [`CaptureMode::Auto`]: tries [`ExtDmabufCapturer`] and falls back to
        ///   [`WaylandShmCapturer`] with a warning if DMA-BUF is unavailable.
        pub fn connect(
            socket: Option<&Path>,
            shm_dir: &Path,
            output_index: usize,
            mode: CaptureMode,
        ) -> Result<Self> {
            match mode {
                CaptureMode::Shm => {
                    WaylandShmCapturer::connect(socket, shm_dir, output_index).map(Self::Shm)
                }
                CaptureMode::Dmabuf => {
                    ExtDmabufCapturer::connect(socket, output_index).map(Self::Dmabuf)
                }
                CaptureMode::Auto => match ExtDmabufCapturer::connect(socket, output_index) {
                    Ok(capturer) => Ok(Self::Dmabuf(capturer)),
                    Err(err) => {
                        warn!(
                            error = %format!("{err:#}"),
                            output_index,
                            "DMA-BUF capture unavailable; falling back to wl_shm"
                        );
                        WaylandShmCapturer::connect(socket, shm_dir, output_index).map(Self::Shm)
                    }
                },
            }
        }

        /// Returns `"dmabuf"` or `"shm"` for the active capture backend.
        pub fn mode_name(&self) -> &'static str {
            match self {
                Self::Dmabuf(_) => "dmabuf",
                Self::Shm(_) => "shm",
            }
        }

        /// Returns a handle that cancels an in-flight or future capture on this capturer.
        pub fn canceller(&self) -> CaptureCancel {
            match self {
                Self::Dmabuf(c) => c.canceller(),
                Self::Shm(c) => c.canceller(),
            }
        }

        /// Width in pixels of the frames this capturer returns.
        pub fn width(&self) -> u32 {
            match self {
                Self::Dmabuf(c) => c.width(),
                Self::Shm(c) => c.width(),
            }
        }

        /// Height in pixels of the frames this capturer returns.
        pub fn height(&self) -> u32 {
            match self {
                Self::Dmabuf(c) => c.height(),
                Self::Shm(c) => c.height(),
            }
        }
    }

    impl FrameCapturer for WaylandCapturer {
        fn backend_type(&self) -> CaptureBackendType {
            match self {
                Self::Dmabuf(c) => c.backend_type(),
                Self::Shm(c) => c.backend_type(),
            }
        }

        fn is_live_display(&self) -> bool {
            match self {
                Self::Dmabuf(c) => c.is_live_display(),
                Self::Shm(c) => c.is_live_display(),
            }
        }

        fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
            match self {
                Self::Dmabuf(c) => c.enumerate_monitors(),
                Self::Shm(c) => c.enumerate_monitors(),
            }
        }

        fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
            match self {
                Self::Dmabuf(c) => c.capture_next_frame(),
                Self::Shm(c) => c.capture_next_frame(),
            }
        }
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
        globals: &GlobalList,
        queue: &mut EventQueue<DmabufCaptureState>,
        state: &mut DmabufCaptureState,
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
            let output: wl_output::WlOutput =
                globals.registry().bind(name, version.min(4), &qh, index);
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

    fn wait_for_constraints(
        queue: &mut EventQueue<DmabufCaptureState>,
        state: &mut DmabufCaptureState,
    ) -> Result<()> {
        while !state.constraints_done {
            if state.stopped {
                bail!("the compositor stopped the capture session before offering a DMA-BUF");
            }
            queue
                .blocking_dispatch(state)
                .context("Wayland dispatch failed while waiting for DMA-BUF constraints")?;
        }
        Ok(())
    }

    fn wait_for_constraints_or_cancelled(
        queue: &mut EventQueue<DmabufCaptureState>,
        state: &mut DmabufCaptureState,
        cancel: &CaptureCancel,
    ) -> Result<()> {
        while !state.constraints_done {
            if state.stopped {
                bail!("the compositor stopped the capture session before offering a DMA-BUF");
            }
            if !dispatch_or_cancelled(queue, state, cancel)
                .context("Wayland dispatch failed while waiting for DMA-BUF constraints")?
            {
                bail!("capture cancelled");
            }
        }
        Ok(())
    }

    fn dispatch_or_cancelled(
        queue: &mut EventQueue<DmabufCaptureState>,
        state: &mut DmabufCaptureState,
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

    fn allocate_dmabuf_buffer(
        gbm: &GbmDevice<File>,
        dmabuf: &ZwpLinuxDmabufV1,
        qh: &QueueHandle<DmabufCaptureState>,
        state: &DmabufCaptureState,
    ) -> Result<DmabufBuffer> {
        let (fourcc, pixel_format, modifier) = pick_dmabuf_format(&state.dmabuf_formats)
            .ok_or_else(|| {
                anyhow!(
                    "compositor offers no 32-bit RGB DMA-BUF format (offered {} format(s))",
                    state.dmabuf_formats.len()
                )
            })?;
        let gbm_format = GbmFormat::try_from(fourcc)
            .map_err(|_| anyhow!("unsupported DRM fourcc {fourcc:#x}"))?;
        let (width, height) = (state.buffer_width, state.buffer_height);
        if width == 0 || height == 0 {
            bail!("compositor offered an empty {width}x{height} DMA-BUF");
        }

        let bo = allocate_gbm_bo(gbm, width, height, gbm_format, modifier)?;
        let dmabuf_fd = bo
            .fd()
            .map_err(|err| anyhow!("failed to export DMA-BUF fd from GBM buffer: {err}"))?;
        let stride = bo.stride();
        let offset = bo.offset(0);
        let actual_modifier: u64 = bo.modifier().into();
        let mod_hi = (actual_modifier >> 32) as u32;
        let mod_lo = actual_modifier as u32;

        let params = dmabuf.create_params(qh, ());
        params.add(dmabuf_fd.as_fd(), 0, offset, stride, mod_hi, mod_lo);
        let wl_buffer = params.create_immed(
            width as i32,
            height as i32,
            fourcc,
            zwp_linux_buffer_params_v1::Flags::empty(),
            qh,
            (),
        );
        params.destroy();

        Ok(DmabufBuffer {
            wl_buffer,
            bo,
            width,
            height,
            pixel_format,
        })
    }

    fn allocate_gbm_bo(
        gbm: &GbmDevice<File>,
        width: u32,
        height: u32,
        format: GbmFormat,
        modifier: u64,
    ) -> Result<BufferObject<()>> {
        let linear_flags = BufferObjectFlags::RENDERING | BufferObjectFlags::LINEAR;
        if modifier == DRM_FORMAT_MOD_LINEAR {
            return gbm
                .create_buffer_object::<()>(width, height, format, linear_flags)
                .or_else(|_| {
                    gbm.create_buffer_object_with_modifiers2::<()>(
                        width,
                        height,
                        format,
                        std::iter::once(Modifier::Linear),
                        BufferObjectFlags::RENDERING,
                    )
                })
                .context("failed to allocate linear GBM buffer object");
        }
        if modifier != DRM_FORMAT_MOD_INVALID {
            return gbm
                .create_buffer_object_with_modifiers2::<()>(
                    width,
                    height,
                    format,
                    std::iter::once(Modifier::from(modifier)),
                    BufferObjectFlags::RENDERING,
                )
                .or_else(|_| {
                    gbm.create_buffer_object_with_modifiers::<()>(
                        width,
                        height,
                        format,
                        std::iter::once(Modifier::from(modifier)),
                    )
                })
                .or_else(|_| gbm.create_buffer_object::<()>(width, height, format, linear_flags))
                .with_context(|| {
                    format!("failed to allocate GBM buffer object with modifier {modifier:#x}")
                });
        }
        gbm.create_buffer_object::<()>(width, height, format, linear_flags)
            .or_else(|_| {
                gbm.create_buffer_object::<()>(width, height, format, BufferObjectFlags::RENDERING)
            })
            .context("failed to allocate GBM buffer object")
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, ExtDmabufCapturer, WaylandCapturer,
    pick_dmabuf_format, resolve_render_node_from_dev_t,
};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::backends::linux_capture::{
        DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_NV12, DRM_FORMAT_XBGR8888,
        DRM_FORMAT_XRGB8888,
    };
    use perigee_protocol::PixelFormat;
    use std::path::Path;

    #[test]
    fn test_pick_dmabuf_format_prefers_bgra_and_linear_modifier() {
        let offered = vec![
            (DRM_FORMAT_ABGR8888, vec![0]),
            (DRM_FORMAT_XRGB8888, vec![0x1234, 0]),
        ];
        assert_eq!(
            pick_dmabuf_format(&offered),
            Some((DRM_FORMAT_XRGB8888, PixelFormat::Bgra8, 0))
        );

        // Falls back to first offered modifier when linear is absent
        let non_linear = vec![(DRM_FORMAT_ARGB8888, vec![0x5678, 0x9abc])];
        assert_eq!(
            pick_dmabuf_format(&non_linear),
            Some((DRM_FORMAT_ARGB8888, PixelFormat::Bgra8, 0x5678))
        );

        // Empty modifier list falls back to DRM_FORMAT_MOD_INVALID
        let empty_mods = vec![(DRM_FORMAT_XBGR8888, vec![])];
        assert_eq!(
            pick_dmabuf_format(&empty_mods),
            Some((
                DRM_FORMAT_XBGR8888,
                PixelFormat::Rgba8,
                DRM_FORMAT_MOD_INVALID
            ))
        );

        // Unsupported-only formats return None
        assert_eq!(pick_dmabuf_format(&[(DRM_FORMAT_NV12, vec![0])]), None);
        assert_eq!(pick_dmabuf_format(&[]), None);
    }

    #[test]
    fn test_resolve_render_node_from_dev_t_handles_empty_and_missing() {
        let tmp =
            std::env::temp_dir().join(format!("perigee-dmabuf-dev-t-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("renderD128"), b"").unwrap();

        // Empty slice -> None
        assert_eq!(resolve_render_node_from_dev_t(&[], &tmp), None);
        // Wrong-length slice -> None
        assert_eq!(resolve_render_node_from_dev_t(&[1, 2, 3], &tmp), None);
        assert_eq!(resolve_render_node_from_dev_t(&[0u8; 16], &tmp), None);
        // Non-matching dev_t against regular file in temp dir -> None
        let bogus_dev: u64 = 0xdead_beef_cafe_babe;
        assert_eq!(
            resolve_render_node_from_dev_t(&bogus_dev.to_ne_bytes(), &tmp),
            None
        );
        // Non-existent dir -> None
        assert_eq!(
            resolve_render_node_from_dev_t(&bogus_dev.to_ne_bytes(), Path::new("/nonexistent/dri")),
            None
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
