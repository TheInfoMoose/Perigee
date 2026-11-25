//! Host accept loop and graceful shutdown.

use crate::backends::{CaptureMode, parse_capture_mode};
use crate::{HostCodec, MediaMode, PerigeeHostDaemon};
use anyhow::{Context, Result, anyhow, bail};
use perigee_net::{
    DEFAULT_PAIRING_PORT, DEFAULT_PAIRING_TTL, IDENTITY_FILE_NAME, NODE_KEY_LEN, NodeIdentity,
    PairingWindow, PeerAuthorizer, QuicServerListener, QuicTransportConfig, ServerAuth,
    TRUSTED_CLIENTS_FILE_NAME, TrustedKeys, VpnInterfaceBinding, default_config_dir,
    node_key_from_hex, node_key_to_hex,
};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Pause between capture-worker frames (10 fps).
#[cfg(target_os = "linux")]
const WORKER_FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// Default UDP socket bind address for `perigee-host` (loopback-only).
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:4433";

/// Default host node name reported by `perigee-host`.
pub const DEFAULT_NODE_NAME: &str = "perigee-host-node";

/// Usage text for `perigee-host --help`.
pub const HOST_USAGE: &str = "\
Usage: perigee-host [run] [OPTIONS]
       perigee-host identity [--config-dir <DIR>]
       perigee-host trust add <KEY> <LABEL> [--config-dir <DIR>]
       perigee-host trust list [--config-dir <DIR>]
       perigee-host trust remove <KEY> [--config-dir <DIR>]
       perigee-host pair [--bind <ADDR>] [--ttl <SECONDS>] [--config-dir <DIR>]
       perigee-host snapshot --out <FILE.ppm> [--output <INDEX>] [--capture-mode <auto|dmabuf|shm>]
       perigee-host input-test
       perigee-host outputs
       perigee-host headless-check

Key management:
  identity              Print this host's node key (created on first use)
  trust add             Allow the client whose node key is KEY (64 hex chars) to connect
  trust list            List trusted client keys
  trust remove          Stop trusting KEY
  pair                  Open a one-client SSH pairing window (default 127.0.0.1:4434, 600 s)
                        and print the host key and a one-time secret for `perigee pair`

Live checks (Linux, run inside the Wayland session):
  snapshot              Capture one frame of output INDEX (default 0) to a PPM file
  input-test            Move the pointer to the centre, click, and type \"perigee\" via /dev/uinput
  outputs               List Wayland compositor outputs with geometry, refresh rate, and scale
  headless-check        Inspect kernel/DRM/vkms readiness for headless operation (no root)

Options:
  --bind <ADDR>         UDP socket bind address (default: 127.0.0.1:4433, loopback-only)
  --interface <NAME>    Bind exclusively to network/VPN interface NAME (fails closed if missing)
  --name <NODE>         Host node identifier (default: perigee-host-node)
  --config-dir <DIR>    Directory holding identity.key and trusted_clients
                        (default: PERIGEE_CONFIG_DIR, else ~/.config/perigee)
  --media <MODE>        standin (default; synthetic frames, logged input), live (Linux:
                        capture this Wayland session, inject input via /dev/uinput), or
                        follow-seat (Linux service: follow seat0's active session, greeter included)
  --codec <CODEC>       raw (default; uncompressed frames) or av1 (GStreamer AV1 encoder,
                        VA-API preferred with software fallback, 30 fps)
  --output <INDEX>      Wayland output index to capture for live/follow-seat (default: 0)
  --capture-mode <MODE> Wayland capture backend for snapshot/capture-worker: auto (default;
                        DMA-BUF with automatic wl_shm fallback), dmabuf, or shm
  -h, --help            Print this help text and exit
  -V, --version         Print version information and exit

Environment variables (overridden by CLI flags):
  PERIGEE_BIND_ADDR       UDP socket bind address (default: 127.0.0.1:4433)
  PERIGEE_BIND_INTERFACE  Network/VPN interface name to bind exclusively (fails closed)
  PERIGEE_CONFIG_DIR      Config directory (identity.key, trusted_clients)
  PERIGEE_CODEC           Video codec: raw (default) or av1
  PERIGEE_OUTPUT          Wayland output index to capture (default: 0)
  PERIGEE_SMOKE_TEST      If set, exit cleanly right after binding the QUIC listener

Only clients whose node key is listed in trusted_clients can connect.";

/// Host daemon configuration resolved from defaults, environment variables, and CLI flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOptions {
    pub bind_addr: SocketAddr,
    pub interface: Option<String>,
    pub node_name: String,
    /// Directory holding `identity.key` and `trusted_clients`; `None` means
    /// [`default_config_dir`].
    pub config_dir: Option<PathBuf>,
    /// Media backends for sessions (`--media`); [`MediaMode::StandIn`] by default.
    pub media: MediaMode,
    /// Video codec for sessions (`--codec`, `PERIGEE_CODEC`); [`HostCodec::Raw`] by default.
    pub codec: HostCodec,
    /// Wayland output index to capture (`--output`, `PERIGEE_OUTPUT`); `0` by default.
    pub output_index: usize,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR
                .parse()
                .expect("DEFAULT_BIND_ADDR is a valid SocketAddr"),
            interface: None,
            node_name: DEFAULT_NODE_NAME.to_string(),
            config_dir: None,
            media: MediaMode::StandIn,
            codec: HostCodec::Raw,
            output_index: 0,
        }
    }
}

fn parse_codec(value: &str) -> Result<HostCodec> {
    match value.to_ascii_lowercase().as_str() {
        "raw" => Ok(HostCodec::Raw),
        "av1" => Ok(HostCodec::Av1),
        other => bail!("unknown codec '{other}': expected raw or av1"),
    }
}

impl HostOptions {
    /// Builds [`HostOptions`] starting from [`HostOptions::default`] and applying
    /// `PERIGEE_BIND_ADDR`, `PERIGEE_BIND_INTERFACE`, `PERIGEE_CODEC`, and `PERIGEE_OUTPUT` from `get`.
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut opts = Self::default();
        if let Some(raw_addr) = get("PERIGEE_BIND_ADDR") {
            opts.bind_addr = raw_addr
                .parse()
                .with_context(|| format!("invalid PERIGEE_BIND_ADDR '{raw_addr}'"))?;
        }
        if let Some(iface) = get("PERIGEE_BIND_INTERFACE")
            && !iface.is_empty()
        {
            opts.interface = Some(iface);
        }
        if let Some(raw_codec) = get("PERIGEE_CODEC")
            && !raw_codec.is_empty()
        {
            opts.codec = parse_codec(&raw_codec)
                .with_context(|| format!("invalid PERIGEE_CODEC '{raw_codec}'"))?;
        }
        if let Some(raw_output) = get("PERIGEE_OUTPUT")
            && !raw_output.is_empty()
        {
            opts.output_index = raw_output
                .parse()
                .with_context(|| format!("invalid PERIGEE_OUTPUT '{raw_output}'"))?;
        }
        Ok(opts)
    }

    /// Produces the [`QuicTransportConfig`] corresponding to `self.interface`.
    pub fn transport_config(&self) -> QuicTransportConfig {
        match &self.interface {
            Some(name) => QuicTransportConfig {
                interface_binding: VpnInterfaceBinding::InterfaceName(name.clone()),
                ..Default::default()
            },
            None => QuicTransportConfig::default(),
        }
    }
}

/// Parsed command-line action for `perigee-host` (and `perigee host`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommand {
    Run(HostOptions),
    Admin(HostAdmin),
    Pair(PairArgs),
    Tool(HostTool),
    Help,
    Version,
}

/// A `perigee-host` key-management subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAdmin {
    /// Print this host's node key.
    Identity { config_dir: Option<PathBuf> },
    /// Trust a client node key under a label.
    TrustAdd {
        config_dir: Option<PathBuf>,
        key: [u8; NODE_KEY_LEN],
        label: String,
    },
    /// List trusted client node keys.
    TrustList { config_dir: Option<PathBuf> },
    /// Stop trusting a client node key.
    TrustRemove {
        config_dir: Option<PathBuf>,
        key: [u8; NODE_KEY_LEN],
    },
}

/// A one-shot local tool (`perigee-host snapshot`, `perigee-host input-test`,
/// `perigee-host outputs`, `perigee-host headless-check`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostTool {
    /// Capture one frame of Wayland output `output` and write it to `out` as a PPM.
    Snapshot {
        out: PathBuf,
        output: usize,
        capture_mode: CaptureMode,
    },
    /// Create the uinput devices, move the pointer to the centre, click, and type `perigee`.
    InputTest,
    /// List Wayland compositor outputs with geometry, refresh rate, and scale.
    Outputs,
    /// Inspect Linux kernel/DRM/`vkms` readiness for headless operation (no root).
    HeadlessCheck,
    /// Internal (started by `--media follow-seat`): capture output `output` of the compositor
    /// in `XDG_RUNTIME_DIR` and stream raw frames on stdout.
    CaptureWorker {
        output: usize,
        capture_mode: CaptureMode,
    },
}

/// Parsed arguments for `perigee-host pair`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairArgs {
    /// TCP address of the pairing window (default `127.0.0.1:4434`).
    pub bind_addr: SocketAddr,
    /// How long the window stays open when nobody pairs.
    pub ttl: Duration,
    pub config_dir: Option<PathBuf>,
}

/// Parses `perigee-host` CLI arguments (`args` excludes the program name) on top of `base`.
pub fn parse_host_args(args: &[String], base: HostOptions) -> Result<HostCommand> {
    match args.first().map(String::as_str) {
        Some("snapshot") => return parse_snapshot_args(&args[1..]).map(HostCommand::Tool),
        Some("input-test") if args.len() == 1 => return Ok(HostCommand::Tool(HostTool::InputTest)),
        Some("input-test") => bail!("input-test takes no arguments"),
        Some("outputs") if args.len() == 1 => return Ok(HostCommand::Tool(HostTool::Outputs)),
        Some("outputs") => bail!("outputs takes no arguments"),
        Some("headless-check") if args.len() == 1 => {
            return Ok(HostCommand::Tool(HostTool::HeadlessCheck));
        }
        Some("headless-check") => bail!("headless-check takes no arguments"),
        Some("capture-worker") => {
            return parse_capture_worker_args(&args[1..]).map(HostCommand::Tool);
        }
        _ => {}
    }
    if args.first().map(String::as_str) == Some("pair") {
        return parse_host_pair(&args[1..]).map(HostCommand::Pair);
    }
    if matches!(args.first().map(String::as_str), Some("identity" | "trust")) {
        return parse_host_admin(args).map(HostCommand::Admin);
    }
    let mut opts = base;
    let mut slice = args;
    if slice.first().map(String::as_str) == Some("run") {
        slice = &slice[1..];
    }

    let mut i = 0;
    while i < slice.len() {
        match slice[i].as_str() {
            "-h" | "--help" => return Ok(HostCommand::Help),
            "-V" | "--version" => return Ok(HostCommand::Version),
            "--bind" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--bind needs a value"))?;
                opts.bind_addr = val
                    .parse()
                    .with_context(|| format!("invalid --bind address '{val}'"))?;
                i += 2;
            }
            "--interface" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--interface needs a value"))?;
                if val.is_empty() {
                    bail!("--interface needs a value");
                }
                opts.interface = Some(val.clone());
                i += 2;
            }
            "--name" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--name needs a value"))?;
                if val.is_empty() {
                    bail!("--name needs a value");
                }
                opts.node_name = val.clone();
                i += 2;
            }
            "--config-dir" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--config-dir needs a value"))?;
                if val.is_empty() {
                    bail!("--config-dir needs a value");
                }
                opts.config_dir = Some(PathBuf::from(val));
                i += 2;
            }
            "--media" => {
                let val = slice.get(i + 1).ok_or_else(|| {
                    anyhow!("--media needs a value: standin, live or follow-seat")
                })?;
                opts.media = match val.as_str() {
                    "standin" => MediaMode::StandIn,
                    "live" => MediaMode::Live,
                    "follow-seat" => MediaMode::FollowSeat,
                    other => {
                        bail!("unknown --media '{other}': expected standin, live or follow-seat")
                    }
                };
                i += 2;
            }
            "--codec" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--codec needs a value: raw or av1"))?;
                opts.codec =
                    parse_codec(val).with_context(|| format!("invalid --codec '{val}'"))?;
                i += 2;
            }
            "--output" => {
                let val = slice
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--output needs a value: output index"))?;
                opts.output_index = val.parse().with_context(|| {
                    format!("invalid --output '{val}': expected an output index")
                })?;
                i += 2;
            }
            other => bail!("unknown argument '{other}'; run with --help"),
        }
    }

    Ok(HostCommand::Run(opts))
}

fn parse_host_admin(args: &[String]) -> Result<HostAdmin> {
    let mut config_dir = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--config-dir needs a value"))?;
                if val.is_empty() {
                    bail!("--config-dir needs a value");
                }
                config_dir = Some(PathBuf::from(val));
                i += 2;
            }
            arg if arg.starts_with('-') => bail!("unknown argument '{arg}'; run with --help"),
            arg => {
                positional.push(arg);
                i += 1;
            }
        }
    }
    match positional.as_slice() {
        ["identity"] => Ok(HostAdmin::Identity { config_dir }),
        ["trust", "list"] => Ok(HostAdmin::TrustList { config_dir }),
        ["trust", "add", key, label] => Ok(HostAdmin::TrustAdd {
            config_dir,
            key: node_key_from_hex(key)?,
            label: (*label).to_string(),
        }),
        ["trust", "remove", key] => Ok(HostAdmin::TrustRemove {
            config_dir,
            key: node_key_from_hex(key)?,
        }),
        _ => bail!(
            "usage: perigee-host identity | trust add <KEY> <LABEL> | trust list | trust remove <KEY>"
        ),
    }
}

fn parse_host_pair(args: &[String]) -> Result<PairArgs> {
    let mut pair = PairArgs {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PAIRING_PORT)),
        ttl: DEFAULT_PAIRING_TTL,
        config_dir: None,
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = match args.get(i + 1) {
            Some(v) if !v.is_empty() => v.as_str(),
            _ if matches!(flag, "--bind" | "--ttl" | "--config-dir") => {
                bail!("{flag} needs a value")
            }
            _ => bail!("unknown argument '{flag}'; run with --help"),
        };
        match flag {
            "--bind" => {
                pair.bind_addr = value
                    .parse()
                    .with_context(|| format!("invalid --bind address '{value}'"))?;
            }
            "--ttl" => {
                let secs: u64 = value
                    .parse()
                    .with_context(|| format!("invalid --ttl '{value}'"))?;
                if secs == 0 {
                    bail!("--ttl must be at least 1 second");
                }
                pair.ttl = Duration::from_secs(secs);
            }
            "--config-dir" => pair.config_dir = Some(PathBuf::from(value)),
            other => bail!("unknown argument '{other}'; run with --help"),
        }
        i += 2;
    }
    Ok(pair)
}

fn parse_snapshot_args(args: &[String]) -> Result<HostTool> {
    let mut out = None;
    let mut output = 0usize;
    let mut capture_mode = CaptureMode::Auto;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let Some(value) = args.get(i + 1).filter(|v| !v.is_empty()) else {
            bail!("{flag} needs a value");
        };
        match flag {
            "--out" => out = Some(PathBuf::from(value)),
            "--output" => {
                output = value.parse().with_context(|| {
                    format!("invalid --output '{value}': expected an output index")
                })?;
            }
            "--capture-mode" => {
                capture_mode = parse_capture_mode(value)
                    .with_context(|| format!("invalid --capture-mode '{value}'"))?;
            }
            other => bail!("unknown argument '{other}'; run with --help"),
        }
        i += 2;
    }
    let out = out.ok_or_else(|| anyhow!("snapshot needs --out <FILE>"))?;
    Ok(HostTool::Snapshot {
        out,
        output,
        capture_mode,
    })
}

fn parse_capture_worker_args(args: &[String]) -> Result<HostTool> {
    let mut output = 0usize;
    let mut capture_mode = CaptureMode::Auto;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let Some(value) = args.get(i + 1).filter(|v| !v.is_empty()) else {
            bail!(
                "usage: perigee-host capture-worker [--output <INDEX>] [--capture-mode <auto|dmabuf|shm>]"
            );
        };
        match flag {
            "--output" => {
                output = value.parse().with_context(|| {
                    format!("invalid --output '{value}': expected an output index")
                })?;
            }
            "--capture-mode" => {
                capture_mode = parse_capture_mode(value)
                    .with_context(|| format!("invalid --capture-mode '{value}'"))?;
            }
            _ => bail!(
                "usage: perigee-host capture-worker [--output <INDEX>] [--capture-mode <auto|dmabuf|shm>]"
            ),
        }
        i += 2;
    }
    Ok(HostTool::CaptureWorker {
        output,
        capture_mode,
    })
}

/// Runs a key-management subcommand and returns the text to print.
pub fn run_host_admin(admin: &HostAdmin) -> Result<String> {
    let dir = |config_dir: &Option<PathBuf>| {
        host_config_dir(&HostOptions {
            config_dir: config_dir.clone(),
            ..HostOptions::default()
        })
    };
    match admin {
        HostAdmin::Identity { config_dir } => {
            let identity =
                NodeIdentity::load_or_create(&dir(config_dir)?.join(IDENTITY_FILE_NAME))?;
            Ok(identity.public_key_hex())
        }
        HostAdmin::TrustAdd {
            config_dir,
            key,
            label,
        } => {
            let trusted = TrustedKeys::load(dir(config_dir)?.join(TRUSTED_CLIENTS_FILE_NAME))?;
            trusted.add(*key, label)?;
            Ok(format!("trusted {} as {label}", node_key_to_hex(key)))
        }
        HostAdmin::TrustList { config_dir } => {
            let trusted = TrustedKeys::load(dir(config_dir)?.join(TRUSTED_CLIENTS_FILE_NAME))?;
            let entries = trusted.entries();
            if entries.is_empty() {
                return Ok("(no trusted clients)".to_string());
            }
            Ok(entries
                .iter()
                .map(|entry| format!("{} {}", node_key_to_hex(&entry.key), entry.label))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        HostAdmin::TrustRemove { config_dir, key } => {
            let trusted = TrustedKeys::load(dir(config_dir)?.join(TRUSTED_CLIENTS_FILE_NAME))?;
            if trusted.remove(key)? {
                Ok(format!("removed {}", node_key_to_hex(key)))
            } else {
                bail!("{} is not a trusted client", node_key_to_hex(key))
            }
        }
    }
}

/// Opens a pairing window, hands the instructions to show to `announce`, and waits for one
/// client. Returns the line to print once the client is trusted; errors when the window expires.
pub async fn run_host_pair(args: &PairArgs, announce: impl FnOnce(&str)) -> Result<String> {
    let config_dir = host_config_dir(&HostOptions {
        config_dir: args.config_dir.clone(),
        ..HostOptions::default()
    })?;
    let identity = NodeIdentity::load_or_create(&config_dir.join(IDENTITY_FILE_NAME))?;
    let window = PairingWindow::bind(
        &identity,
        config_dir.join(TRUSTED_CLIENTS_FILE_NAME),
        args.bind_addr,
        args.ttl,
    )
    .await?;
    let addr = window.local_addr()?;
    let key = identity.public_key_hex();
    announce(&format!(
        "Pairing window open on {addr} for {} s.\nhost key: {key}\nsecret:   {}\nOn the client run:\n  perigee pair <HOST> {key} {} --label <NAME> --pair-port {}",
        args.ttl.as_secs(),
        window.secret(),
        window.secret(),
        addr.port()
    ));
    match window.run().await? {
        Some(client) => Ok(format!(
            "trusted {} as {}",
            node_key_to_hex(&client.key),
            client.label
        )),
        None => bail!("pairing window expired; nobody paired"),
    }
}

/// Reads `reader` on a background thread until EOF or error (discarding all bytes), then calls
/// `on_close` once. Used by `capture-worker` to exit when its parent closes stdin.
#[cfg(target_os = "linux")]
fn watch_lifeline<R: std::io::Read + Send + 'static>(
    mut reader: R,
    on_close: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        on_close();
    })
}

/// Fits a captured frame to the wire budget so `RawVideoEncoder` passes it through unscaled.
#[cfg(target_os = "linux")]
fn worker_frame_message(
    frame: &crate::CapturedFrame,
) -> anyhow::Result<perigee_protocol::WireMessage> {
    let fitted = crate::fit_raw_frame(frame, crate::RAW_FRAME_MAX_BYTES)?;
    Ok(perigee_protocol::WireMessage::Video(
        perigee_protocol::VideoFrameMessage {
            frame_id: fitted.frame_index,
            monitor_id: fitted.display_id,
            timestamp_us: fitted.timestamp_us,
            codec: perigee_protocol::VideoCodec::RawBgra,
            is_keyframe: true,
            width: fitted.width,
            height: fitted.height,
            stride: fitted.stride,
            format: fitted.pixel_format,
            payload: fitted.data,
        },
    ))
}

/// Runs a one-shot local tool and returns the line to print.
pub fn run_host_tool(tool: &HostTool) -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        use crate::HostInputInjector;
        use crate::backends::FrameCapturer;
        use perigee_protocol::{ElementState, InputEvent, KeyModifiers, MouseButtonCode};

        match tool {
            HostTool::Snapshot {
                out,
                output,
                capture_mode,
            } => {
                let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .context(
                        "snapshot needs XDG_RUNTIME_DIR (run it inside the Wayland session)",
                    )?;
                let mut capturer = crate::backends::WaylandCapturer::connect(
                    None,
                    &runtime_dir,
                    *output,
                    *capture_mode,
                )?;
                let frame = capturer.capture_next_frame()?;
                let ppm = crate::encode_ppm(
                    frame.width,
                    frame.height,
                    frame.stride,
                    frame.pixel_format,
                    &frame.data,
                )?;
                std::fs::write(out, ppm)
                    .with_context(|| format!("failed to write {}", out.display()))?;
                Ok(format!(
                    "wrote {}x{} frame of output {output} ({}) to {}",
                    frame.width,
                    frame.height,
                    capturer.mode_name(),
                    out.display()
                ))
            }
            HostTool::InputTest => {
                let injector = crate::backends::UinputInjector::new(1920, 1080)?;
                // Give the compositor time to pick up the new devices before sending events.
                std::thread::sleep(Duration::from_secs(1));
                injector.inject_input(&InputEvent::MouseMove {
                    monitor_id: 0,
                    x: 0.5,
                    y: 0.5,
                    is_absolute: true,
                })?;
                for state in [ElementState::Pressed, ElementState::Released] {
                    injector.inject_input(&InputEvent::MouseButton {
                        button: MouseButtonCode::Left,
                        state,
                        x: 0.5,
                        y: 0.5,
                    })?;
                }
                // evdev KEY_P, KEY_E, KEY_R, KEY_I, KEY_G, KEY_E, KEY_E
                for scancode in [25u32, 18, 19, 23, 34, 18, 18] {
                    for state in [ElementState::Pressed, ElementState::Released] {
                        injector.inject_input(&InputEvent::KeyEvent {
                            key_code: 0,
                            scancode,
                            state,
                            modifiers: KeyModifiers::default(),
                        })?;
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
                Ok("moved the pointer to the centre, clicked, and typed \"perigee\"".to_string())
            }
            HostTool::Outputs => {
                let monitors = crate::backends::wayland_shm::list_wayland_outputs(None)?;
                Ok(crate::backends::wayland_shm::format_monitor_list(&monitors))
            }
            HostTool::HeadlessCheck => {
                let report = crate::headless::probe_headless_system();
                Ok(crate::headless::format_headless_report(&report))
            }
            HostTool::CaptureWorker {
                output,
                capture_mode,
            } => {
                use std::io::Write;

                let _lifeline = watch_lifeline(std::io::stdin(), || {
                    info!("capture-worker: parent closed stdin; exiting");
                    std::process::exit(0);
                });

                let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .context("capture-worker needs XDG_RUNTIME_DIR")?;
                let socket = crate::seat::find_wayland_socket(&runtime_dir)?;
                let mut capturer = crate::backends::WaylandCapturer::connect(
                    Some(&socket),
                    &runtime_dir,
                    *output,
                    *capture_mode,
                )?;
                let mut stdout = std::io::stdout().lock();
                if let Ok(monitors) = capturer.enumerate_monitors()
                    && !monitors.is_empty()
                {
                    let layout_msg = perigee_protocol::WireMessage::Topology(
                        perigee_protocol::TopologyMessage::DisplayLayout { monitors },
                    );
                    let framed = perigee_protocol::encode_length_prefixed(&layout_msg)?;
                    if stdout
                        .write_all(&framed)
                        .and_then(|()| stdout.flush())
                        .is_err()
                    {
                        return Ok("capture-worker: the reader closed the pipe".to_string());
                    }
                }
                loop {
                    let frame = capturer.capture_next_frame()?;
                    let message = worker_frame_message(&frame)?;
                    let framed = perigee_protocol::encode_length_prefixed(&message)?;
                    if stdout
                        .write_all(&framed)
                        .and_then(|()| stdout.flush())
                        .is_err()
                    {
                        return Ok("capture-worker: the reader closed the pipe".to_string());
                    }
                    std::thread::sleep(WORKER_FRAME_INTERVAL);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tool;
        bail!("snapshot, input-test, outputs, and headless-check are Linux only")
    }
}

/// Resolves the `perigee-host` command line. `--help` and `--version` never read the
/// environment; only a run applies `PERIGEE_BIND_ADDR` / `PERIGEE_BIND_INTERFACE` (read
/// through `get`) underneath the flags.
pub fn resolve_host_command(
    args: &[String],
    get: impl Fn(&str) -> Option<String>,
) -> Result<HostCommand> {
    match parse_host_args(args, HostOptions::default())? {
        HostCommand::Run(_) => parse_host_args(args, HostOptions::from_env_with(get)?),
        other => Ok(other),
    }
}

/// Trusts the client keys listed in a trust file. The file is re-read on every handshake, so
/// `perigee-host trust add` takes effect without a restart. A missing file trusts nobody; an
/// unreadable or malformed file trusts nobody and logs a warning.
#[derive(Debug)]
pub struct TrustFileAuthorizer {
    path: PathBuf,
}

impl TrustFileAuthorizer {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl PeerAuthorizer for TrustFileAuthorizer {
    fn is_authorized(&self, node_key: &[u8; NODE_KEY_LEN]) -> bool {
        match TrustedKeys::load(self.path.clone()) {
            Ok(trusted) => trusted.contains(node_key),
            Err(e) => {
                warn!(error = %format!("{e:#}"), "Refusing client: trust file is unreadable");
                false
            }
        }
    }
}

/// The host's config directory: `options.config_dir`, else [`default_config_dir`].
pub fn host_config_dir(options: &HostOptions) -> Result<PathBuf> {
    options
        .config_dir
        .clone()
        .or_else(default_config_dir)
        .ok_or_else(|| {
            anyhow!("no Perigee config directory: set PERIGEE_CONFIG_DIR or pass --config-dir")
        })
}

/// Loads (or creates) the host's node identity, auto-detects the capture backend, constructs a
/// [`PerigeeHostDaemon`], and binds a mutual-TLS [`QuicServerListener`] that admits only the
/// clients listed in `<config dir>/trusted_clients`.
pub fn bind_host(options: &HostOptions) -> Result<(Arc<PerigeeHostDaemon>, QuicServerListener)> {
    let config_dir = host_config_dir(options)?;
    let identity = Arc::new(NodeIdentity::load_or_create(
        &config_dir.join(IDENTITY_FILE_NAME),
    )?);
    let authorizer = Arc::new(TrustFileAuthorizer::new(
        config_dir.join(TRUSTED_CLIENTS_FILE_NAME),
    ));
    let backend = PerigeeHostDaemon::auto_detect_backend();
    let mut daemon = PerigeeHostDaemon::new(options.node_name.clone(), backend)
        .with_codec(options.codec)
        .with_output_index(options.output_index);
    daemon.media = options.media;
    let daemon = Arc::new(daemon);
    let listener = QuicServerListener::bind_mtls(
        options.bind_addr,
        options.transport_config(),
        ServerAuth {
            identity: identity.clone(),
            authorizer,
        },
    )?;
    info!(
        local_addr = ?listener.local_addr()?,
        backend = ?daemon.backend,
        node_key = %identity.public_key_hex(),
        config_dir = %config_dir.display(),
        "Perigee Host Daemon listening for mutual-TLS QUIC connections"
    );
    Ok((daemon, listener))
}

/// Runs the host accept loop until `shutdown` resolves or the listener endpoint closes,
/// then stops all active sessions and drains the QUIC endpoint.
pub async fn run_host(
    daemon: Arc<PerigeeHostDaemon>,
    listener: QuicServerListener,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            conn = listener.accept() => {
                match conn {
                    Some(conn) => {
                        let remote = conn.connection.remote_address();
                        let daemon_clone = daemon.clone();
                        tokio::spawn(async move {
                            match daemon_clone.serve_connection(conn).await {
                                Ok(session) => {
                                    info!(
                                        %remote,
                                        session_id = %session.session_id,
                                        "Provisioned host session for remote viewer"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        %remote,
                                        error = %format!("{e:#}"),
                                        "Failed to serve incoming QUIC connection"
                                    );
                                }
                            }
                        });
                    }
                    None => break,
                }
            }
        }
    }

    daemon.shutdown_all_sessions().await;
    listener.endpoint.close(0u32.into(), b"host shutdown");
    match tokio::time::timeout(Duration::from_secs(2), listener.endpoint.wait_idle()).await {
        Ok(()) => info!("QUIC endpoint drained cleanly"),
        Err(_) => warn!("Timed out waiting for QUIC endpoint to drain after shutdown"),
    }
    info!("host stopped");
    Ok(())
}

/// Waits for an OS termination signal (`SIGINT` / Ctrl-C, or `SIGTERM` on Unix).
///
/// Shared by the host daemon and the `perigee` CLI client.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    res = tokio::signal::ctrl_c() => {
                        match res {
                            Ok(()) => info!("Received SIGINT (Ctrl-C), shutting down"),
                            Err(e) => warn!(error = %e, "Failed listening for SIGINT, shutting down"),
                        }
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, shutting down");
                    }
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "SIGTERM handler unavailable; only Ctrl-C will stop this process"
                );
                match tokio::signal::ctrl_c().await {
                    Ok(()) => info!("Received SIGINT (Ctrl-C), shutting down"),
                    Err(e) => warn!(error = %e, "Failed listening for SIGINT, shutting down"),
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        match tokio::signal::ctrl_c().await {
            Ok(()) => info!("Received SIGINT (Ctrl-C), shutting down"),
            Err(e) => warn!(error = %e, "Failed listening for SIGINT, shutting down"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureBackendType;
    use perigee_net::{ClientAuth, LoopbackPeers, QuicClientTransport, QuicTransportConfig};
    use perigee_protocol::{HandshakeMessage, VideoCodec, WireMessage};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn test_run_host_stops_sessions_and_closes_connections_on_shutdown() {
        let peers = LoopbackPeers::generate().unwrap();
        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind QUIC listener");
        let addr = listener.local_addr().unwrap();

        let daemon = Arc::new(PerigeeHostDaemon::new(
            "host-test".into(),
            CaptureBackendType::WaylandPipeWire,
        ));

        let (tx, rx) = oneshot::channel::<()>();
        let daemon_host = daemon.clone();
        let host_task = tokio::spawn(async move {
            run_host(daemon_host, listener, async {
                let _ = rx.await;
            })
            .await
        });

        let client = QuicClientTransport::new(QuicTransportConfig::default());
        let client_conn = client
            .connect_mtls(addr, peers.client_auth())
            .await
            .expect("Client failed to connect");

        let hello = HandshakeMessage::ClientHello {
            protocol_version: 1,
            client_id: "host-shutdown-client".to_string(),
            client_name: "Host Shutdown Viewer".to_string(),
            supported_codecs: vec![VideoCodec::AV1],
        };
        client_conn
            .send_uni_message(&WireMessage::Handshake(hello))
            .await
            .expect("Failed to send ClientHello");

        let auth_resp =
            tokio::time::timeout(Duration::from_secs(5), client_conn.recv_uni_message())
                .await
                .expect("Timed out waiting for AuthResponse")
                .expect("Failed to receive AuthResponse");
        assert!(matches!(
            auth_resp,
            WireMessage::Handshake(HandshakeMessage::AuthResponse {
                success: true,
                session_token: Some(_),
                ..
            })
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            while daemon.active_session_count().await != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Timed out waiting for active_session_count() == 1");

        let _ = tx.send(());

        let host_res = tokio::time::timeout(Duration::from_secs(5), host_task)
            .await
            .expect("run_host timed out on shutdown")
            .expect("run_host task panicked");
        assert!(host_res.is_ok());

        assert_eq!(daemon.active_session_count().await, 0);

        tokio::time::timeout(Duration::from_secs(5), client_conn.connection.closed())
            .await
            .expect("Client connection did not close within 5s after host shutdown");
    }

    #[tokio::test]
    async fn test_run_host_returns_when_endpoint_closes() {
        let peers = LoopbackPeers::generate().unwrap();
        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind QUIC listener");

        let daemon = Arc::new(PerigeeHostDaemon::new(
            "host-close-test".into(),
            CaptureBackendType::WaylandPipeWire,
        ));

        let endpoint_clone = listener.endpoint.clone();
        endpoint_clone.close(0u32.into(), b"pre-closed");

        let host_task =
            tokio::spawn(async move { run_host(daemon, listener, std::future::pending()).await });

        let host_res = tokio::time::timeout(Duration::from_secs(5), host_task)
            .await
            .expect("run_host timed out when endpoint closed")
            .expect("run_host task panicked");
        assert!(host_res.is_ok());
    }

    #[test]
    fn test_host_options_defaults_and_env() {
        let defaults = HostOptions::from_env_with(|_| None).unwrap();
        assert_eq!(defaults, HostOptions::default());
        assert_eq!(
            defaults.bind_addr,
            "127.0.0.1:4433".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(defaults.interface, None);
        assert_eq!(defaults.node_name, DEFAULT_NODE_NAME);

        let configured = HostOptions::from_env_with(|k| match k {
            "PERIGEE_BIND_ADDR" => Some("192.0.2.10:5000".to_string()),
            "PERIGEE_BIND_INTERFACE" => Some("wg0".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            configured.bind_addr,
            "192.0.2.10:5000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(configured.interface, Some("wg0".to_string()));
        assert_eq!(configured.node_name, DEFAULT_NODE_NAME);

        let err = HostOptions::from_env_with(|k| match k {
            "PERIGEE_BIND_ADDR" => Some("not-an-addr".to_string()),
            _ => None,
        })
        .unwrap_err();
        let err_text = format!("{err:#}");
        assert!(
            err_text.contains("PERIGEE_BIND_ADDR"),
            "Expected error containing PERIGEE_BIND_ADDR, got: {err_text}"
        );
    }

    #[test]
    fn test_parse_host_args() {
        let base = HostOptions::default();

        assert_eq!(
            parse_host_args(&[], base.clone()).unwrap(),
            HostCommand::Run(base.clone())
        );

        let args: Vec<String> = [
            "run",
            "--bind",
            "192.0.2.10:5000",
            "--interface",
            "wg0",
            "--name",
            "lab",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            parse_host_args(&args, base.clone()).unwrap(),
            HostCommand::Run(HostOptions {
                bind_addr: "192.0.2.10:5000".parse().unwrap(),
                interface: Some("wg0".to_string()),
                node_name: "lab".to_string(),
                config_dir: None,
                media: MediaMode::StandIn,
                codec: HostCodec::Raw,
                output_index: 0,
            })
        );

        assert_eq!(
            parse_host_args(&["--help".to_string()], base.clone()).unwrap(),
            HostCommand::Help
        );
        assert_eq!(
            parse_host_args(&["-V".to_string()], base.clone()).unwrap(),
            HostCommand::Version
        );

        assert!(parse_host_args(&["--bind".to_string()], base.clone()).is_err());

        let unknown_err = parse_host_args(&["--frobnicate".to_string()], base).unwrap_err();
        assert!(
            unknown_err.to_string().contains("unknown argument"),
            "Expected error containing 'unknown argument', got: {unknown_err}"
        );
    }

    #[test]
    fn test_parse_config_dir_flag() {
        let args = ["--config-dir".to_string(), "/tmp/perigee-test".to_string()];
        assert_eq!(
            parse_host_args(&args, HostOptions::default()).unwrap(),
            HostCommand::Run(HostOptions {
                config_dir: Some(PathBuf::from("/tmp/perigee-test")),
                ..HostOptions::default()
            })
        );
        assert!(parse_host_args(&["--config-dir".to_string()], HostOptions::default()).is_err());
    }

    #[test]
    fn test_trust_file_authorizer_rereads_file() {
        let dir = std::env::temp_dir().join(format!("perigee-host-authz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TRUSTED_CLIENTS_FILE_NAME);
        let authorizer = TrustFileAuthorizer::new(path.clone());
        let key = [0x44; NODE_KEY_LEN];
        assert!(!authorizer.is_authorized(&key));
        TrustedKeys::load(path.clone())
            .unwrap()
            .add(key, "laptop")
            .unwrap();
        assert!(authorizer.is_authorized(&key));
        std::fs::write(&path, "not a trust line\n").unwrap();
        assert!(!authorizer.is_authorized(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_bind_host_admits_client_from_trust_file() {
        let dir = std::env::temp_dir().join(format!("perigee-host-bind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = HostOptions {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            config_dir: Some(dir.clone()),
            ..HostOptions::default()
        };
        let (_daemon, listener) = bind_host(&options).expect("bind_host should succeed");
        let addr = listener.local_addr().unwrap();
        let host_key = NodeIdentity::load_or_create(&dir.join(IDENTITY_FILE_NAME))
            .unwrap()
            .public_key();

        let client = Arc::new(NodeIdentity::generate().unwrap());
        TrustedKeys::load(dir.join(TRUSTED_CLIENTS_FILE_NAME))
            .unwrap()
            .add(client.public_key(), "test-client")
            .unwrap();

        let server_task = tokio::spawn(async move {
            let conn = listener.accept().await;
            (listener, conn.map(|c| c.peer_node_key()))
        });
        let _client_conn = QuicClientTransport::new(QuicTransportConfig::default())
            .connect_mtls(
                addr,
                ClientAuth {
                    identity: client.clone(),
                    host_key,
                },
            )
            .await
            .expect("a client listed in trusted_clients should connect");
        let (_listener, seen) = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("server task timed out")
            .expect("server task panicked");
        assert_eq!(seen, Some(Some(client.public_key())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_host_admin() {
        let base = HostOptions::default;
        let key_hex = "ab".repeat(NODE_KEY_LEN);
        let args = |items: &[&str]| items.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_host_args(&args(&["identity"]), base()).unwrap(),
            HostCommand::Admin(HostAdmin::Identity { config_dir: None })
        );
        assert_eq!(
            parse_host_args(
                &args(&[
                    "trust",
                    "add",
                    &key_hex,
                    "laptop",
                    "--config-dir",
                    "/tmp/perigee-test"
                ]),
                base()
            )
            .unwrap(),
            HostCommand::Admin(HostAdmin::TrustAdd {
                config_dir: Some(PathBuf::from("/tmp/perigee-test")),
                key: [0xab; NODE_KEY_LEN],
                label: "laptop".to_string(),
            })
        );
        assert_eq!(
            parse_host_args(&args(&["trust", "list"]), base()).unwrap(),
            HostCommand::Admin(HostAdmin::TrustList { config_dir: None })
        );
        assert_eq!(
            parse_host_args(&args(&["trust", "remove", &key_hex]), base()).unwrap(),
            HostCommand::Admin(HostAdmin::TrustRemove {
                config_dir: None,
                key: [0xab; NODE_KEY_LEN],
            })
        );
        assert!(parse_host_args(&args(&["trust", "add", "nothex", "laptop"]), base()).is_err());
        assert!(parse_host_args(&args(&["trust"]), base()).is_err());
    }

    #[test]
    fn test_run_host_admin_round_trip() {
        let dir = std::env::temp_dir().join(format!("perigee-host-admin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config_dir = Some(dir.clone());

        let printed = run_host_admin(&HostAdmin::Identity {
            config_dir: config_dir.clone(),
        })
        .unwrap();
        let identity = NodeIdentity::load_or_create(&dir.join(IDENTITY_FILE_NAME)).unwrap();
        assert_eq!(printed, identity.public_key_hex());

        let key = [0x55; NODE_KEY_LEN];
        run_host_admin(&HostAdmin::TrustAdd {
            config_dir: config_dir.clone(),
            key,
            label: "laptop".to_string(),
        })
        .unwrap();
        let listed = run_host_admin(&HostAdmin::TrustList {
            config_dir: config_dir.clone(),
        })
        .unwrap();
        assert_eq!(listed, format!("{} laptop", node_key_to_hex(&key)));

        run_host_admin(&HostAdmin::TrustRemove {
            config_dir: config_dir.clone(),
            key,
        })
        .unwrap();
        assert!(
            run_host_admin(&HostAdmin::TrustRemove {
                config_dir: config_dir.clone(),
                key,
            })
            .is_err()
        );
        assert_eq!(
            run_host_admin(&HostAdmin::TrustList { config_dir }).unwrap(),
            "(no trusted clients)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_transport_config_follows_interface() {
        let default_opts = HostOptions::default();
        assert_eq!(
            default_opts.transport_config().interface_binding,
            VpnInterfaceBinding::DefaultSystemRoute
        );

        let iface_opts = HostOptions {
            interface: Some("wg0".to_string()),
            ..HostOptions::default()
        };
        assert_eq!(
            iface_opts.transport_config().interface_binding,
            VpnInterfaceBinding::InterfaceName("wg0".to_string())
        );
    }

    #[test]
    fn test_resolve_host_command_ignores_env_for_help() {
        let bad_env = |k: &str| (k == "PERIGEE_BIND_ADDR").then(|| "not-an-addr".to_string());
        assert_eq!(
            resolve_host_command(&["--help".to_string()], bad_env).unwrap(),
            HostCommand::Help
        );
        assert_eq!(
            resolve_host_command(&["-V".to_string()], bad_env).unwrap(),
            HostCommand::Version
        );
        let err = resolve_host_command(&[], bad_env).unwrap_err();
        assert!(format!("{err:#}").contains("PERIGEE_BIND_ADDR"));

        let env = |k: &str| (k == "PERIGEE_BIND_ADDR").then(|| "192.0.2.10:5000".to_string());
        assert_eq!(
            resolve_host_command(&[], env).unwrap(),
            HostCommand::Run(HostOptions {
                bind_addr: "192.0.2.10:5000".parse().unwrap(),
                ..HostOptions::default()
            })
        );
        let args = ["--bind".to_string(), "127.0.0.1:0".to_string()];
        assert_eq!(
            resolve_host_command(&args, env).unwrap(),
            HostCommand::Run(HostOptions {
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                ..HostOptions::default()
            })
        );
    }

    #[test]
    fn test_parse_host_pair_args() {
        let args: Vec<String> = [
            "pair",
            "--bind",
            "192.0.2.10:5000",
            "--ttl",
            "30",
            "--config-dir",
            "/tmp/perigee-test/pair",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            parse_host_args(&args, HostOptions::default()).unwrap(),
            HostCommand::Pair(PairArgs {
                bind_addr: "192.0.2.10:5000".parse().unwrap(),
                ttl: Duration::from_secs(30),
                config_dir: Some(PathBuf::from("/tmp/perigee-test/pair")),
            })
        );
        assert_eq!(
            parse_host_args(&["pair".to_string()], HostOptions::default()).unwrap(),
            HostCommand::Pair(PairArgs {
                bind_addr: "127.0.0.1:4434".parse().unwrap(),
                ttl: DEFAULT_PAIRING_TTL,
                config_dir: None,
            })
        );
        let zero: Vec<String> = ["pair", "--ttl", "0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_host_args(&zero, HostOptions::default()).is_err());
        let bogus: Vec<String> = ["pair", "--bogus"].iter().map(|s| s.to_string()).collect();
        assert!(parse_host_args(&bogus, HostOptions::default()).is_err());
    }

    #[tokio::test]
    async fn test_run_host_pair_expires_without_client() {
        let dir = std::env::temp_dir().join(format!("perigee-host-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let args = PairArgs {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            ttl: Duration::from_secs(1),
            config_dir: Some(dir.clone()),
        };
        let mut announced = String::new();
        let err = run_host_pair(&args, |text| announced = text.to_string())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("expired"), "got: {err:#}");
        assert!(announced.contains("secret:"));
        assert!(announced.contains("perigee pair <HOST>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_media_flag() {
        let live: Vec<String> = ["run", "--media", "live"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            parse_host_args(&live, HostOptions::default()).unwrap(),
            HostCommand::Run(HostOptions {
                media: MediaMode::Live,
                ..HostOptions::default()
            })
        );
        assert_eq!(HostOptions::default().media, MediaMode::StandIn);
        let bad: Vec<String> = ["--media", "dmabuf"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let follow: Vec<String> = ["--media", "follow-seat"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            parse_host_args(&follow, HostOptions::default()).unwrap(),
            HostCommand::Run(HostOptions {
                media: MediaMode::FollowSeat,
                ..HostOptions::default()
            })
        );
        assert!(parse_host_args(&bad, HostOptions::default()).is_err());
    }

    #[test]
    fn test_parse_host_args_codec_flag_and_env() {
        let s = |items: &[&str]| items.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();

        // CLI --codec av1 and --codec raw
        assert_eq!(
            parse_host_args(&s(&["--codec", "av1"]), HostOptions::default()).unwrap(),
            HostCommand::Run(HostOptions {
                codec: HostCodec::Av1,
                ..HostOptions::default()
            })
        );
        assert_eq!(
            parse_host_args(&s(&["--codec", "raw"]), HostOptions::default()).unwrap(),
            HostCommand::Run(HostOptions {
                codec: HostCodec::Raw,
                ..HostOptions::default()
            })
        );

        // PERIGEE_CODEC=av1 (and case-insensitive AV1)
        let env_av1 = |k: &str| (k == "PERIGEE_CODEC").then(|| "av1".to_string());
        assert_eq!(
            resolve_host_command(&[], env_av1).unwrap(),
            HostCommand::Run(HostOptions {
                codec: HostCodec::Av1,
                ..HostOptions::default()
            })
        );
        let env_upper = |k: &str| (k == "PERIGEE_CODEC").then(|| "AV1".to_string());
        assert_eq!(
            resolve_host_command(&[], env_upper).unwrap(),
            HostCommand::Run(HostOptions {
                codec: HostCodec::Av1,
                ..HostOptions::default()
            })
        );

        // CLI flag overrides PERIGEE_CODEC
        assert_eq!(
            resolve_host_command(&s(&["--codec", "raw"]), env_av1).unwrap(),
            HostCommand::Run(HostOptions {
                codec: HostCodec::Raw,
                ..HostOptions::default()
            })
        );

        // Invalid --codec bogus and missing value error
        let err = parse_host_args(&s(&["--codec", "bogus"]), HostOptions::default()).unwrap_err();
        assert!(
            format!("{err:#}").contains("bogus"),
            "Expected error mentioning bogus, got: {err:#}"
        );
        assert!(parse_host_args(&s(&["--codec"]), HostOptions::default()).is_err());

        // Invalid PERIGEE_CODEC errors on run, but --help still succeeds
        let bad_env = |k: &str| (k == "PERIGEE_CODEC").then(|| "bogus".to_string());
        assert!(resolve_host_command(&[], bad_env).is_err());
        assert_eq!(
            resolve_host_command(&s(&["--help"]), bad_env).unwrap(),
            HostCommand::Help
        );
    }

    #[test]
    fn test_parse_host_output_flag_and_env() {
        let s = |items: &[&str]| items.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();

        assert_eq!(HostOptions::default().output_index, 0);

        let env_two = |k: &str| (k == "PERIGEE_OUTPUT").then(|| "2".to_string());
        let from_env = HostOptions::from_env_with(env_two).unwrap();
        assert_eq!(from_env.output_index, 2);

        let bad_env = |k: &str| (k == "PERIGEE_OUTPUT").then(|| "abc".to_string());
        let err = HostOptions::from_env_with(bad_env).unwrap_err();
        assert!(
            format!("{err:#}").contains("PERIGEE_OUTPUT"),
            "Expected error naming PERIGEE_OUTPUT, got: {err:#}"
        );

        assert_eq!(
            resolve_host_command(&s(&["--output", "1"]), env_two).unwrap(),
            HostCommand::Run(HostOptions {
                output_index: 1,
                ..HostOptions::default()
            })
        );

        let missing_err = parse_host_args(&s(&["--output"]), HostOptions::default()).unwrap_err();
        assert!(
            format!("{missing_err:#}").contains("--output"),
            "Expected error naming --output, got: {missing_err:#}"
        );

        let invalid_err =
            parse_host_args(&s(&["--output", "abc"]), HostOptions::default()).unwrap_err();
        assert!(
            format!("{invalid_err:#}").contains("--output"),
            "Expected error naming --output, got: {invalid_err:#}"
        );
    }

    #[test]
    fn test_parse_host_tools() {
        let snap: Vec<String> = [
            "snapshot",
            "--out",
            "/tmp/perigee-test/frame.ppm",
            "--output",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            parse_host_args(&snap, HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::Snapshot {
                out: PathBuf::from("/tmp/perigee-test/frame.ppm"),
                output: 1,
                capture_mode: CaptureMode::Auto,
            })
        );
        let no_out: Vec<String> = ["snapshot"].iter().map(|s| s.to_string()).collect();
        assert!(parse_host_args(&no_out, HostOptions::default()).is_err());
        assert_eq!(
            parse_host_args(&["input-test".to_string()], HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::InputTest)
        );
        let extra: Vec<String> = ["input-test", "now"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_host_args(&extra, HostOptions::default()).is_err());
    }

    #[test]
    fn test_parse_outputs_tool() {
        assert_eq!(
            parse_host_args(&["outputs".to_string()], HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::Outputs)
        );
        let extra: Vec<String> = ["outputs", "extra"].iter().map(|s| s.to_string()).collect();
        assert!(parse_host_args(&extra, HostOptions::default()).is_err());
    }

    #[test]
    fn test_parse_capture_worker() {
        assert_eq!(
            parse_host_args(&["capture-worker".to_string()], HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::CaptureWorker {
                output: 0,
                capture_mode: CaptureMode::Auto,
            })
        );
        let two: Vec<String> = ["capture-worker", "--output", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            parse_host_args(&two, HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::CaptureWorker {
                output: 2,
                capture_mode: CaptureMode::Auto,
            })
        );
        let bad: Vec<String> = ["capture-worker", "--bogus"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_host_args(&bad, HostOptions::default()).is_err());
    }

    #[test]
    fn test_parse_capture_mode_and_tool_args() {
        assert_eq!(parse_capture_mode("auto").unwrap(), CaptureMode::Auto);
        assert_eq!(parse_capture_mode("AUTO").unwrap(), CaptureMode::Auto);
        assert_eq!(parse_capture_mode("dmabuf").unwrap(), CaptureMode::Dmabuf);
        assert_eq!(parse_capture_mode("DmaBuf").unwrap(), CaptureMode::Dmabuf);
        assert_eq!(parse_capture_mode("shm").unwrap(), CaptureMode::Shm);
        assert_eq!(parse_capture_mode("SHM").unwrap(), CaptureMode::Shm);
        assert!(parse_capture_mode("vulkan").is_err());

        let s = |items: &[&str]| items.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();

        assert_eq!(
            parse_host_args(
                &s(&[
                    "snapshot",
                    "--out",
                    "/tmp/a.ppm",
                    "--capture-mode",
                    "dmabuf"
                ]),
                HostOptions::default(),
            )
            .unwrap(),
            HostCommand::Tool(HostTool::Snapshot {
                out: PathBuf::from("/tmp/a.ppm"),
                output: 0,
                capture_mode: CaptureMode::Dmabuf,
            })
        );
        assert_eq!(
            parse_host_args(
                &s(&["snapshot", "--out", "/tmp/a.ppm"]),
                HostOptions::default()
            )
            .unwrap(),
            HostCommand::Tool(HostTool::Snapshot {
                out: PathBuf::from("/tmp/a.ppm"),
                output: 0,
                capture_mode: CaptureMode::Auto,
            })
        );
        assert_eq!(
            parse_host_args(
                &s(&["capture-worker", "--output", "1", "--capture-mode", "shm"]),
                HostOptions::default(),
            )
            .unwrap(),
            HostCommand::Tool(HostTool::CaptureWorker {
                output: 1,
                capture_mode: CaptureMode::Shm,
            })
        );
        assert_eq!(
            parse_host_args(&s(&["capture-worker"]), HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::CaptureWorker {
                output: 0,
                capture_mode: CaptureMode::Auto,
            })
        );
        assert!(
            parse_host_args(
                &s(&["snapshot", "--out", "/tmp/a.ppm", "--capture-mode", "bogus"]),
                HostOptions::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_headless_check_tool() {
        let s = |items: &[&str]| items.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();

        assert_eq!(
            parse_host_args(&s(&["headless-check"]), HostOptions::default()).unwrap(),
            HostCommand::Tool(HostTool::HeadlessCheck)
        );
        assert!(
            parse_host_args(&s(&["headless-check", "--extra"]), HostOptions::default()).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_watch_lifeline_fires_on_eof() {
        use std::io::Write;
        use std::sync::mpsc;

        let (reader, mut writer) = std::io::pipe().unwrap();
        let (tx, rx) = mpsc::channel();
        let handle = watch_lifeline(reader, move || {
            let _ = tx.send(());
        });

        writer.write_all(b"hello").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "lifeline should not fire while stdin is still open"
        );

        drop(writer);
        rx.recv_timeout(Duration::from_secs(1))
            .expect("lifeline should fire when stdin reaches EOF");
        handle.join().unwrap();
        assert!(
            rx.try_recv().is_err(),
            "lifeline callback should fire exactly once"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_worker_frame_message_fits_wire_budget() {
        let frame = crate::CapturedFrame {
            frame_index: 7,
            timestamp_us: 1234,
            display_id: 0,
            width: 5120,
            height: 1440,
            stride: 20480,
            pixel_format: perigee_protocol::PixelFormat::Rgba8,
            data: vec![0u8; 20480 * 1440],
            is_dmabuf_or_metal_handle: false,
        };
        let message = worker_frame_message(&frame).unwrap();
        let framed = perigee_protocol::encode_length_prefixed(&message).unwrap();
        assert!(!framed.is_empty());
        match message {
            WireMessage::Video(video) => {
                assert_eq!(video.width, 1706);
                assert_eq!(video.height, 480);
                assert_eq!(video.frame_id, 7);
                assert_eq!(video.codec, VideoCodec::RawBgra);
                assert!(video.is_keyframe);
                assert!(video.payload.len() <= crate::RAW_FRAME_MAX_BYTES);
            }
            other => panic!("expected WireMessage::Video, got {other:?}"),
        }
    }
}
