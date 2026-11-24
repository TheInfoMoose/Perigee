//! Real Linux input injection through `/dev/uinput`.
//!
//! Status: real (Phase 5a). Creates two kernel virtual devices, "Perigee virtual keyboard" and
//! "Perigee virtual pointer" (absolute X/Y 0..=65535, five buttons, vertical and horizontal
//! wheel). Compositors and the greeter read them like hardware, so no consent dialog is
//! involved. cosmic-comp maps absolute motion onto the active output only; multi-monitor is
//! Phase 5b. Needs write access to `/dev/uinput` (root, or the `perigee` group through the
//! packaged udev rule).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

use perigee_protocol::{ElementState, InputEvent, MonitorInfo, WheelUnit};

use crate::HostInputInjector;
use crate::backends::linux_input::{
    EVDEV_BTN_EXTRA, EVDEV_BTN_LEFT, mouse_button_to_evdev, normalize_pointer_coordinates,
    translate_key_to_evdev_scancode,
};

/// Upper bound of the virtual pointer's absolute X and Y axes.
pub const UINPUT_ABS_MAX: i32 = 65_535;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BUS_VIRTUAL: u16 = 0x06;
/// Highest key code the virtual keyboard advertises (`KEY_MICMUTE`).
const KEYBOARD_MAX_KEY: u16 = 248;

// ioctl request numbers from linux/uinput.h: _IO('U', 1), _IO('U', 2), _IOW('U', 100..=103, int).
const UI_DEV_CREATE: u32 = 0x5501;
const UI_DEV_DESTROY: u32 = 0x5502;
const UI_SET_EVBIT: u32 = 0x4004_5564;
const UI_SET_KEYBIT: u32 = 0x4004_5565;
const UI_SET_RELBIT: u32 = 0x4004_5566;
const UI_SET_ABSBIT: u32 = 0x4004_5567;

// encode_input_event writes the 64-bit `struct input_event` layout.
const _: () = assert!(std::mem::size_of::<libc::input_event>() == 24);

/// Encodes one kernel `struct input_event`: a zero 16-byte timestamp (the kernel stamps it),
/// then type, code and value in native byte order.
pub fn encode_input_event(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[16..18].copy_from_slice(&kind.to_ne_bytes());
    out[18..20].copy_from_slice(&code.to_ne_bytes());
    out[20..24].copy_from_slice(&value.to_ne_bytes());
    out
}

/// Encodes the legacy `struct uinput_user_dev` (1116 bytes) written before `UI_DEV_CREATE`:
/// name (79 bytes max, NUL-padded), bus `BUS_VIRTUAL`, vendor 0x1d6b, `product`, version 1, and
/// `abs_max` as the maximum of `ABS_X` and `ABS_Y` (0 for a device without absolute axes).
pub fn encode_uinput_user_dev(name: &str, product: u16, abs_max: i32) -> Vec<u8> {
    let mut out = vec![0u8; 1116];
    let bytes = name.as_bytes();
    let len = bytes.len().min(79);
    out[..len].copy_from_slice(&bytes[..len]);
    out[80..82].copy_from_slice(&BUS_VIRTUAL.to_ne_bytes());
    out[82..84].copy_from_slice(&0x1d6b_u16.to_ne_bytes());
    out[84..86].copy_from_slice(&product.to_ne_bytes());
    out[86..88].copy_from_slice(&1_u16.to_ne_bytes());
    // 88..92 ff_effects_max = 0; absmax[64] starts at 92, then absmin, absfuzz, absflat.
    for axis in [ABS_X, ABS_Y] {
        let at = 92 + usize::from(axis) * 4;
        out[at..at + 4].copy_from_slice(&abs_max.to_ne_bytes());
    }
    out
}

struct VirtualDevice {
    file: File,
}

impl VirtualDevice {
    fn create(name: &str, product: u16, setup: &[(u32, u16)], abs_max: i32) -> Result<Self> {
        let mut file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("failed to open /dev/uinput (needs root or the perigee group)")?;
        let fd = file.as_raw_fd();
        for &(request, value) in setup {
            // SAFETY: `fd` is the open /dev/uinput descriptor owned by `file`, which outlives
            // this call. UI_SET_*BIT takes an int by value, so no pointer is passed and the
            // kernel keeps nothing from this process's memory.
            let rc = unsafe { libc::ioctl(fd, request as _, libc::c_int::from(value)) };
            if rc < 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("uinput ioctl {request:#x} {value} failed"));
            }
        }
        file.write_all(&encode_uinput_user_dev(name, product, abs_max))
            .context("failed to write the uinput device description")?;
        // SAFETY: same open descriptor as above; UI_DEV_CREATE takes no argument.
        let rc = unsafe { libc::ioctl(fd, UI_DEV_CREATE as _) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_DEV_CREATE failed");
        }
        Ok(Self { file })
    }

    fn emit(&self, events: &[(u16, u16, i32)]) -> Result<()> {
        let mut buf = Vec::with_capacity((events.len() + 1) * 24);
        for &(kind, code, value) in events {
            buf.extend_from_slice(&encode_input_event(kind, code, value));
        }
        buf.extend_from_slice(&encode_input_event(EV_SYN, SYN_REPORT, 0));
        (&self.file)
            .write_all(&buf)
            .context("failed to write uinput events")
    }
}

impl Drop for VirtualDevice {
    fn drop(&mut self) {
        // SAFETY: the descriptor is still open (the File closes after this body runs);
        // UI_DEV_DESTROY takes no argument.
        let _ = unsafe { libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY as _) };
    }
}

/// Computes the union bounding box `(min_x, min_y, total_width, total_height)` across all
/// monitors with `width > 0 && height > 0`. Returns `None` if no valid monitors exist.
pub fn compute_virtual_desktop_bounds(monitors: &[MonitorInfo]) -> Option<(i32, i32, u32, u32)> {
    let mut min_x = i64::MAX;
    let mut min_y = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = i64::MIN;
    let mut found = false;

    for monitor in monitors {
        if monitor.width == 0 || monitor.height == 0 {
            continue;
        }
        let x0 = i64::from(monitor.x);
        let y0 = i64::from(monitor.y);
        let x1 = x0 + i64::from(monitor.width);
        let y1 = y0 + i64::from(monitor.height);
        min_x = min_x.min(x0);
        min_y = min_y.min(y0);
        max_x = max_x.max(x1);
        max_y = max_y.max(y1);
        found = true;
    }

    if !found {
        return None;
    }

    let total_width = u32::try_from(max_x - min_x).ok()?;
    let total_height = u32::try_from(max_y - min_y).ok()?;
    if total_width == 0 || total_height == 0 {
        return None;
    }
    let origin_x = i32::try_from(min_x).ok()?;
    let origin_y = i32::try_from(min_y).ok()?;
    Some((origin_x, origin_y, total_width, total_height))
}

/// Maps output-local pointer coordinates `(x, y)` on `target` into `0..=UINPUT_ABS_MAX`
/// across the union bounding box of `monitors`.
///
/// Coordinates in `0.0..=1.0` are treated as normalized output-local coordinates; otherwise
/// they are treated as pixel coordinates within `target.width x target.height` and clamped
/// to `[0.0, 1.0]`.
pub fn map_output_pointer_to_abs(
    x: f64,
    y: f64,
    target: &MonitorInfo,
    monitors: &[MonitorInfo],
) -> Result<(i32, i32)> {
    if !x.is_finite() || !y.is_finite() {
        bail!("non-finite pointer coordinates: ({x}, {y})");
    }
    if target.width == 0 || target.height == 0 {
        bail!(
            "invalid target monitor dimensions: {}x{}",
            target.width,
            target.height
        );
    }
    let Some((min_x, min_y, total_width, total_height)) = compute_virtual_desktop_bounds(monitors)
    else {
        bail!("no valid monitors for virtual desktop bounds");
    };

    let (norm_x, norm_y) = if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
        (x, y)
    } else {
        (
            (x / f64::from(target.width)).clamp(0.0, 1.0),
            (y / f64::from(target.height)).clamp(0.0, 1.0),
        )
    };

    let global_x = f64::from(target.x) + norm_x * f64::from(target.width);
    let global_y = f64::from(target.y) + norm_y * f64::from(target.height);
    let desktop_norm_x = ((global_x - f64::from(min_x)) / f64::from(total_width)).clamp(0.0, 1.0);
    let desktop_norm_y = ((global_y - f64::from(min_y)) / f64::from(total_height)).clamp(0.0, 1.0);

    let abs_x = (desktop_norm_x * f64::from(UINPUT_ABS_MAX)).round() as i32;
    let abs_y = (desktop_norm_y * f64::from(UINPUT_ABS_MAX)).round() as i32;
    Ok((
        abs_x.clamp(0, UINPUT_ABS_MAX),
        abs_y.clamp(0, UINPUT_ABS_MAX),
    ))
}

/// Validates and rounds relative pointer motion deltas `(dx, dy)` for `EV_REL` injection.
pub fn map_relative_pointer_delta(dx: f64, dy: f64) -> Result<(i32, i32)> {
    if !dx.is_finite() || !dy.is_finite() {
        bail!("non-finite relative pointer delta: ({dx}, {dy})");
    }
    let rel_x = dx.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let rel_y = dy.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    Ok((rel_x, rel_y))
}

#[derive(Debug, Default)]
struct OutputLayout {
    monitors: Vec<MonitorInfo>,
    active_monitor_id: u32,
}

/// Real [`HostInputInjector`] backed by two `/dev/uinput` virtual devices.
pub struct UinputInjector {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    screen_width: AtomicU32,
    screen_height: AtomicU32,
    output_layout: Mutex<OutputLayout>,
}

impl UinputInjector {
    /// Creates the virtual keyboard and pointer. Pixel-space pointer coordinates are scaled
    /// against `screen_width` x `screen_height` until [`UinputInjector::set_screen_size`].
    pub fn new(screen_width: u32, screen_height: u32) -> Result<Self> {
        let mut key_setup = vec![(UI_SET_EVBIT, EV_KEY)];
        key_setup.extend((1..=KEYBOARD_MAX_KEY).map(|code| (UI_SET_KEYBIT, code)));
        let keyboard = VirtualDevice::create("Perigee virtual keyboard", 0x0001, &key_setup, 0)?;

        let mut pointer_setup = vec![
            (UI_SET_EVBIT, EV_KEY),
            (UI_SET_EVBIT, EV_ABS),
            (UI_SET_EVBIT, EV_REL),
            (UI_SET_ABSBIT, ABS_X),
            (UI_SET_ABSBIT, ABS_Y),
            (UI_SET_RELBIT, REL_X),
            (UI_SET_RELBIT, REL_Y),
            (UI_SET_RELBIT, REL_WHEEL),
            (UI_SET_RELBIT, REL_HWHEEL),
        ];
        pointer_setup
            .extend((EVDEV_BTN_LEFT..=EVDEV_BTN_EXTRA).map(|code| (UI_SET_KEYBIT, code as u16)));
        let pointer = VirtualDevice::create(
            "Perigee virtual pointer",
            0x0002,
            &pointer_setup,
            UINPUT_ABS_MAX,
        )?;

        info!(
            screen_width,
            screen_height, "Created /dev/uinput virtual keyboard and pointer"
        );
        Ok(Self {
            keyboard,
            pointer,
            screen_width: AtomicU32::new(screen_width.max(1)),
            screen_height: AtomicU32::new(screen_height.max(1)),
            output_layout: Mutex::new(OutputLayout::default()),
        })
    }

    /// Updates the size that pixel-space pointer coordinates are scaled against (the size of
    /// the frames the client sees).
    pub fn set_screen_size(&self, width: u32, height: u32) {
        self.screen_width.store(width.max(1), Ordering::Relaxed);
        self.screen_height.store(height.max(1), Ordering::Relaxed);
    }

    /// Stores the live multi-monitor layout and updates `screen_width`/`screen_height` from
    /// the active monitor when present.
    pub fn set_output_layout(&self, monitors: Vec<MonitorInfo>, active_monitor_id: u32) {
        if let Some(active) = monitors
            .iter()
            .find(|m| m.monitor_id == active_monitor_id)
            .filter(|m| m.width > 0 && m.height > 0)
        {
            self.set_screen_size(active.width, active.height);
        }
        let mut layout = self
            .output_layout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        layout.monitors = monitors;
        layout.active_monitor_id = active_monitor_id;
    }

    fn move_to(&self, x: f64, y: f64, is_absolute: bool) -> Result<()> {
        let width = self.screen_width.load(Ordering::Relaxed);
        let height = self.screen_height.load(Ordering::Relaxed);
        let coords = normalize_pointer_coordinates(x, y, is_absolute, width, height)?;
        let abs_x = (coords.norm_x * f64::from(UINPUT_ABS_MAX)).round() as i32;
        let abs_y = (coords.norm_y * f64::from(UINPUT_ABS_MAX)).round() as i32;
        self.pointer
            .emit(&[(EV_ABS, ABS_X, abs_x), (EV_ABS, ABS_Y, abs_y)])
    }
}

fn element_value(state: ElementState) -> i32 {
    match state {
        ElementState::Pressed => 1,
        ElementState::Released => 0,
    }
}

impl HostInputInjector for UinputInjector {
    fn inject_input(&self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::MouseMove {
                monitor_id,
                x,
                y,
                is_absolute,
            } => {
                if !*is_absolute {
                    let (dx, dy) = map_relative_pointer_delta(*x, *y)?;
                    let mut events = Vec::with_capacity(2);
                    if dx != 0 {
                        events.push((EV_REL, REL_X, dx));
                    }
                    if dy != 0 {
                        events.push((EV_REL, REL_Y, dy));
                    }
                    if events.is_empty() {
                        return Ok(());
                    }
                    return self.pointer.emit(&events);
                }

                let mapped_abs = {
                    let layout = self
                        .output_layout
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if layout.monitors.len() > 1 {
                        let target = layout
                            .monitors
                            .iter()
                            .find(|m| m.monitor_id == *monitor_id)
                            .or_else(|| {
                                layout
                                    .monitors
                                    .iter()
                                    .find(|m| m.monitor_id == layout.active_monitor_id)
                            })
                            .unwrap_or(&layout.monitors[0]);
                        Some(map_output_pointer_to_abs(*x, *y, target, &layout.monitors)?)
                    } else {
                        None
                    }
                };

                if let Some((abs_x, abs_y)) = mapped_abs {
                    self.pointer
                        .emit(&[(EV_ABS, ABS_X, abs_x), (EV_ABS, ABS_Y, abs_y)])
                } else {
                    self.move_to(*x, *y, true)
                }
            }
            InputEvent::MouseButton { button, state, .. } => {
                let code = mouse_button_to_evdev(*button);
                let Ok(code) = u16::try_from(code) else {
                    bail!("mouse button code {code:#x} is out of range");
                };
                self.pointer.emit(&[(EV_KEY, code, element_value(*state))])
            }
            InputEvent::MouseWheel {
                delta_x,
                delta_y,
                unit,
            } => {
                let steps = |delta: f32| match unit {
                    WheelUnit::Line => delta.round() as i32,
                    WheelUnit::Pixel => (delta / 15.0).round() as i32,
                };
                let mut events = Vec::new();
                let vertical = steps(*delta_y);
                if vertical != 0 {
                    events.push((EV_REL, REL_WHEEL, vertical));
                }
                let horizontal = steps(*delta_x);
                if horizontal != 0 {
                    events.push((EV_REL, REL_HWHEEL, horizontal));
                }
                if events.is_empty() {
                    return Ok(());
                }
                self.pointer.emit(&events)
            }
            InputEvent::KeyEvent {
                key_code,
                scancode,
                state,
                ..
            } => {
                let code = translate_key_to_evdev_scancode(*key_code, *scancode);
                match u16::try_from(code) {
                    Ok(code) if (1..=KEYBOARD_MAX_KEY).contains(&code) => {
                        self.keyboard.emit(&[(EV_KEY, code, element_value(*state))])
                    }
                    _ => {
                        debug!(
                            key_code,
                            scancode, code, "Dropping a key with no evdev keyboard code"
                        );
                        Ok(())
                    }
                }
            }
            InputEvent::TouchEvent { .. } | InputEvent::TextInput { .. } => {
                debug!("Touch and text input are not injected through uinput yet");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_input_event_layout() {
        let bytes = encode_input_event(EV_KEY, 30, 1);
        assert_eq!(bytes.len(), std::mem::size_of::<libc::input_event>());
        assert_eq!(&bytes[..16], &[0u8; 16]);
        assert_eq!(&bytes[16..18], &EV_KEY.to_ne_bytes());
        assert_eq!(&bytes[18..20], &30u16.to_ne_bytes());
        assert_eq!(&bytes[20..24], &1i32.to_ne_bytes());
    }

    #[test]
    fn test_encode_uinput_user_dev_layout() {
        let dev = encode_uinput_user_dev("Perigee virtual pointer", 0x0002, UINPUT_ABS_MAX);
        assert_eq!(dev.len(), 1116);
        assert_eq!(&dev[..23], b"Perigee virtual pointer");
        assert_eq!(dev[23], 0);
        assert_eq!(&dev[80..82], &BUS_VIRTUAL.to_ne_bytes());
        assert_eq!(&dev[84..86], &0x0002u16.to_ne_bytes());
        assert_eq!(&dev[92..96], &UINPUT_ABS_MAX.to_ne_bytes());
        assert_eq!(&dev[96..100], &UINPUT_ABS_MAX.to_ne_bytes());
        assert_eq!(&dev[100..104], &[0u8; 4]);
        let long = encode_uinput_user_dev(&"x".repeat(100), 1, 0);
        assert_eq!(long[78], b'x');
        assert_eq!(long[79], 0);
    }

    fn make_monitor(id: u32, x: i32, y: i32, width: u32, height: u32) -> MonitorInfo {
        MonitorInfo {
            monitor_id: id,
            name: format!("DP-{id}"),
            x,
            y,
            width,
            height,
            scale_factor: 1.0,
            refresh_rate_hz: 60,
            is_primary: id == 1,
        }
    }

    #[test]
    fn test_compute_virtual_desktop_bounds_handles_negative_offsets_and_empty() {
        assert_eq!(compute_virtual_desktop_bounds(&[]), None);

        let zero_sized = vec![
            make_monitor(1, 0, 0, 0, 1080),
            make_monitor(2, 0, 0, 1920, 0),
        ];
        assert_eq!(compute_virtual_desktop_bounds(&zero_sized), None);

        let side_by_side = vec![
            make_monitor(1, -1920, 0, 1920, 1080),
            make_monitor(2, 0, 0, 1920, 1080),
        ];
        assert_eq!(
            compute_virtual_desktop_bounds(&side_by_side),
            Some((-1920, 0, 3840, 1080))
        );
    }

    #[test]
    fn test_map_output_pointer_to_abs_dual_monitor_and_invalid_input() {
        let monitors = vec![
            make_monitor(1, 0, 0, 1920, 1080),
            make_monitor(2, 1920, 0, 1920, 1080),
        ];

        let (x1, y1) = map_output_pointer_to_abs(0.5, 0.5, &monitors[0], &monitors).unwrap();
        assert!((x1 - 16_384).abs() <= 1, "unexpected x1={x1}");
        assert!((y1 - 32_768).abs() <= 1, "unexpected y1={y1}");

        let (x2, y2) = map_output_pointer_to_abs(0.5, 0.5, &monitors[1], &monitors).unwrap();
        assert!((x2 - 49_151).abs() <= 1, "unexpected x2={x2}");
        assert!((y2 - 32_768).abs() <= 1, "unexpected y2={y2}");

        // Pixel-space coordinates on monitor 2 also map to the same desktop point.
        let (px2, py2) = map_output_pointer_to_abs(960.0, 540.0, &monitors[1], &monitors).unwrap();
        assert_eq!((px2, py2), (x2, y2));

        assert!(map_output_pointer_to_abs(f64::NAN, 0.5, &monitors[0], &monitors).is_err());
        assert!(map_output_pointer_to_abs(0.5, f64::INFINITY, &monitors[0], &monitors).is_err());
    }

    #[test]
    fn test_map_relative_pointer_delta_rounds_and_rejects_non_finite() {
        assert_eq!(map_relative_pointer_delta(12.6, -4.2).unwrap(), (13, -4));
        assert_eq!(map_relative_pointer_delta(0.0, 0.0).unwrap(), (0, 0));
        assert!(map_relative_pointer_delta(f64::INFINITY, 0.0).is_err());
        assert!(map_relative_pointer_delta(0.0, f64::NAN).is_err());
    }
}
