# Headless Linux (`vkms`) Spike Checklist & Upstream `cosmic-comp` Issue Draft

`cosmic-comp` currently requires at least one connected DRM output on its KMS backend and does not yet provide native virtual/headless output creation (`docs/audit/15-headless-greeter-research.md`). Until upstream `cosmic-comp` supports virtual outputs, headless Linux hosts rely on either the Linux kernel `vkms` virtual KMS driver or a hardware HDMI/DisplayPort EDID dummy plug.

This document provides:
1. A step-by-step live verification checklist for testing `vkms` with `cosmic-comp` and `perigee-host`.
2. A ready-to-file upstream issue draft for `pop-os/cosmic-comp`.

---

## 1. Live `vkms` Spike Checklist

### Prerequisites
- A Linux host running COSMIC (`cosmic-comp` KMS backend) with no physical monitor attached (or with physical outputs disconnected for the headless test).
- `perigee-host` built and installed in `$PATH` (or invoked via `cargo run -p perigee-host --locked -- ...`).
- Root/sudo access for `modprobe vkms` and `configfs` setup (never required for `perigee-host headless-check`).

### Step 1: Baseline probe (no root)
Run the read-only probe before making any kernel changes:

```bash
perigee-host headless-check
```

Record:
- Kernel release (`/proc/sys/kernel/osrelease`)
- Whether `vkms` is already loaded (`/sys/module/vkms`)
- Whether `vkms` configfs is available (`/sys/kernel/config/vkms`)
- All DRM connectors and their statuses under `/sys/class/drm`

When zero physical monitors are attached, `headless-check` should report `Status: not ready (0 connected DRM connectors)` and recommend `sudo modprobe vkms`.

### Step 2: Load `vkms` and re-probe
Load the virtual KMS kernel module:

```bash
sudo modprobe vkms
perigee-host headless-check
```

Verify:
- `vkms module: loaded`
- A virtual connector (e.g., `card1-Virtual-1 [virtual]: connected`) appears in the connector list.
- `Status: ready (1 connected DRM connector(s))`

To persist `vkms` across reboots once verified:
```bash
echo vkms | sudo tee /etc/modules-load.d/vkms.conf
```

### Step 3: Dynamic virtual connectors via `configfs` (Linux >= 6.12)
On Linux kernels >= 6.12 with `CONFIG_DRM_VKMS` configfs support enabled (verify `vkms configfs: available` in `perigee-host headless-check`), test creating a custom virtual output dynamically:

```bash
sudo mount -t configfs none /sys/kernel/config 2>/dev/null || true
# Load vkms without the default static device if configuring solely via configfs:
# sudo modprobe vkms create_default_dev=0

sudo mkdir -p /sys/kernel/config/vkms/perigee0
sudo mkdir -p /sys/kernel/config/vkms/perigee0/planes/plane0
echo 1 | sudo tee /sys/kernel/config/vkms/perigee0/planes/plane0/type   # primary plane
sudo mkdir -p /sys/kernel/config/vkms/perigee0/crtcs/crtc0
sudo mkdir -p /sys/kernel/config/vkms/perigee0/encoders/encoder0
sudo mkdir -p /sys/kernel/config/vkms/perigee0/connectors/connector0

sudo ln -s /sys/kernel/config/vkms/perigee0/crtcs/crtc0 \
  /sys/kernel/config/vkms/perigee0/planes/plane0/possible_crtcs/crtc0
sudo ln -s /sys/kernel/config/vkms/perigee0/crtcs/crtc0 \
  /sys/kernel/config/vkms/perigee0/encoders/encoder0/possible_crtcs/crtc0
sudo ln -s /sys/kernel/config/vkms/perigee0/encoders/encoder0 \
  /sys/kernel/config/vkms/perigee0/connectors/connector0/possible_encoders/encoder0

echo 1 | sudo tee /sys/kernel/config/vkms/perigee0/enabled
perigee-host headless-check
```

Record whether `cosmic-comp` hot-plugs the new virtual connector and whether custom modes can be applied without restarting `cosmic-comp`.

### Step 4: Verify `cosmic-comp` KMS + `perigee-host` capture
With `vkms` active and `cosmic-comp` running on KMS (either `cosmic-greeter` or a logged-in user session):

1. Enumerate Wayland outputs:
   ```bash
   perigee-host outputs
   ```
   Verify the `vkms` output appears with expected geometry (`+x+y`), resolution, refresh rate, and scale.

2. Capture a test frame via DMA-BUF (`auto` and explicit `dmabuf`) and SHM:
   ```bash
   perigee-host snapshot --out /tmp/perigee-vkms-auto.ppm --output 0 --capture-mode auto
   perigee-host snapshot --out /tmp/perigee-vkms-dmabuf.ppm --output 0 --capture-mode dmabuf
   perigee-host snapshot --out /tmp/perigee-vkms-shm.ppm --output 0 --capture-mode shm
   ```
   Verify the output PPM files are non-empty, valid P6 PPM images, and reflect the live desktop/greeter rather than black/corrupt frames. Record whether `dmabuf` succeeds on `vkms` or falls back to `shm`.

### Step 5: Hybrid GPU caveats
When a physical GPU (AMD, Intel, or NVIDIA) is present alongside `vkms`:
- `vkms` creates a separate DRM card (e.g., `/dev/dri/card1`) without a hardware GPU render node; `cosmic-comp`'s KMS/GBM/GLES renderer may use the physical GPU's render node for compositing and blit/scanout to `vkms`, or fail modesetting if buffer modifiers/formats are incompatible.
- Test and record:
  - GPU vendor(s) and driver versions (`lspci -nnk | grep -A3 -E 'VGA|3D'`).
  - Whether `cosmic-comp` starts cleanly with 0 physical monitors + `vkms` on AMD/Intel/NVIDIA.
  - Any `journalctl -u cosmic-greeter` or `cosmic-comp` errors mentioning `"Backend initialized without output"`, GBM allocation, or DRM modifier negotiation.
  - Whether `--capture-mode dmabuf` works or requires `--capture-mode shm` when composited across `vkms` + dGPU.

### Step 6: Hardware EDID dummy-plug fallback
If `vkms` fails to modeset or render under `cosmic-comp` on a target GPU/kernel (for example, on proprietary NVIDIA drivers or older kernels lacking `vkms` configfs):
- Attach a passive HDMI or DisplayPort EDID dummy plug (typically supporting 1080p/1440p/4K modes).
- Re-run `perigee-host headless-check` to confirm the physical connector reports `connected`.
- Re-run `perigee-host outputs` and `perigee-host snapshot` to verify normal hardware-accelerated DMA-BUF capture.

---

## 2. Upstream Issue Draft for `pop-os/cosmic-comp`

> Ready to file at https://github.com/pop-os/cosmic-comp/issues/new (fill in the live `vkms` spike results from Section 1 before submitting).

### Title
Feature request: Native headless/virtual outputs (with dynamic resize) and unattended system-authorized EIS / virtual pointer support for remote desktop

### Body

#### Summary
We are building an unattended Wayland remote desktop host (`perigee-host`) targeting COSMIC (`cosmic-comp` + `cosmic-greeter`). Unattended capture already works cleanly on `cosmic-comp` via `ext-image-copy-capture-v1` + `ext-image-capture-source-v1` (including at `cosmic-greeter` and on `ext-session-lock-v1` lock screens). However, two gaps prevent a clean, compositor-native remote desktop experience on COSMIC without kernel workarounds (`vkms` and `/dev/uinput`):

1. **Native headless / virtual outputs with dynamic resize on KMS** (even when zero physical monitors are attached).
2. **System/polkit-authorized unattended EIS (or virtual pointer) access** supporting global multi-output absolute coordinates, including at `cosmic-greeter`.

---

#### 1. Native headless / virtual outputs & dynamic `ResizeRequest`

**Current behavior (audited against `cosmic-comp@0681eaf`):**
- `cosmic-comp` only implements `x11`, `winit`, and `kms` backends (`src/backend/mod.rs:25-46`) and exits with `"Backend initialized without output"` if no DRM connector is connected (`src/backend/mod.rs:49-56`).
- On KMS, `cosmic-comp` only binds physical DRM connectors (`src/backend/kms/mod.rs:148-240`) and `apply_config_for_outputs` only accepts modes already advertised by the connector's EDID (`src/backend/kms/mod.rs:1111-1129`, `"Unable to find matching mode"`).
- Neither `cosmic-output-management-unstable-v1` (`cosmic-protocols@815b2c5` `unstable/cosmic-output-management-unstable-v1.xml:36-103`) nor `xdg-desktop-portal-cosmic` (`xdg-desktop-portal-cosmic@7b0864c` `src/screencast.rs:486-489`, which only advertises `SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW`, not `SOURCE_TYPE_VIRTUAL`) can create virtual outputs.

**Impact:**
- Headless COSMIC machines (servers, workstations with monitors powered off/unplugged) cannot start `cosmic-comp` on KMS without loading kernel `vkms` or attaching a hardware EDID dummy plug.
- Remote desktop clients cannot dynamically resize a virtual output to match the viewer window's resolution and scale (`ResizeRequest`).

**Ask:**
- Support creating and dynamically resizing virtual/headless outputs in `cosmic-comp` (including when zero physical DRM connectors are attached on KMS, e.g., via a `cosmic-output-management` / Wayland extension for unsandboxed or authorized clients, and/or `SOURCE_TYPE_VIRTUAL` in `xdg-desktop-portal-cosmic`).

---

#### 2. System/polkit-authorized unattended EIS / virtual pointer (including at `cosmic-greeter`)

**Current behavior (audited against `cosmic-comp@0681eaf` and `cosmic-greeter@20af063`):**
- `cosmic-comp` registers `VirtualKeyboardManagerState` (`zwp_virtual_keyboard_manager_v1`, `src/state.rs:718`) but does **not** expose `zwlr_virtual_pointer_manager_v1`.
- `cosmic-comp`'s `libei` / EIS broker (`com.system76.CosmicComp.Ei`, `src/dbus/ei.rs:17-51, 87-107`) lives exclusively on the **session** D-Bus and hardcodes `ALLOWED_NAMES` to `org.freedesktop.impl.portal.desktop.cosmic` and `com.system76.CosmicOSK`, rejecting all other callers with `AccessDenied`.
- `cosmic-greeter` launches `exec cosmic-comp cosmic-greeter` (`cosmic-greeter@20af063` `cosmic-greeter-start.sh:1-4`, `cosmic-greeter.toml:1-10`) without a user session D-Bus or `xdg-desktop-portal-cosmic`, so portal `RemoteDesktop` + `ConnectToEIS` is unavailable at the login screen.
- As a result, unattended remote desktop daemons must fall back to `/dev/uinput`. However, `cosmic-comp`'s non-EI `PointerMotionAbsolute` handler maps `libinput` `EV_ABS` strictly onto `seat.active_output()` (`src/input/mod.rs:791-801`), whereas `InputBackendId::Ei(_)` already supports global multi-output `EiRegion` absolute coordinates (`src/libei.rs:31-58`, `src/input/mod.rs:766-790`). Crossing monitors via `/dev/uinput` therefore requires relative `EV_REL` emulation (`src/input/mod.rs:739-744`) instead of direct absolute coordinates.

**Ask:**
- Expose a compositor-level or polkit/system-authorized EIS endpoint (or `zwlr_virtual_pointer_manager_v1` / `ext-virtual-pointer`) that works in both `cosmic-greeter` (kiosk/greeter mode) and logged-in user sessions, allowing authorized unattended remote-desktop daemons to inject keyboard and global multi-output absolute pointer events via `cosmic-comp`'s existing `EiRegion` path (`src/libei.rs:31-58`, `src/input/mod.rs:766-790`) without `/dev/uinput` workarounds.
- Additionally, please confirm whether `cosmic-greeter` is expected to continue exposing `$XDG_RUNTIME_DIR/wayland-*` (`src/lib.rs:303-315`) with `ext-image-copy-capture-v1` for unsandboxed peers, or if a dedicated remote-login hook is planned.

---

#### Live `vkms` spike observations (fill in from User Step 9G)
- Kernel version: `<from perigee-host headless-check>`
- GPU / driver: `<e.g., AMD Radeon / Intel / NVIDIA>`
- `vkms` + `cosmic-comp` KMS startup: `<pass / fail + error log>`
- `perigee-host outputs` on `vkms`: `<output geometry>`
- `perigee-host snapshot --capture-mode dmabuf` vs `shm` on `vkms`: `<results>`
