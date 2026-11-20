//! Host accept loop and graceful shutdown.

use crate::PerigeeHostDaemon;
use anyhow::{Context, Result, anyhow, bail};
use perigee_net::{
    IDENTITY_FILE_NAME, NODE_KEY_LEN, NodeIdentity, PeerAuthorizer, QuicServerListener,
    QuicTransportConfig, ServerAuth, TRUSTED_CLIENTS_FILE_NAME, TrustedKeys, VpnInterfaceBinding,
    default_config_dir, node_key_from_hex, node_key_to_hex,
};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

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

Key management:
  identity              Print this host's node key (created on first use)
  trust add             Allow the client whose node key is KEY (64 hex chars) to connect
  trust list            List trusted client keys
  trust remove          Stop trusting KEY

Options:
  --bind <ADDR>         UDP socket bind address (default: 127.0.0.1:4433, loopback-only)
  --interface <NAME>    Bind exclusively to network/VPN interface NAME (fails closed if missing)
  --name <NODE>         Host node identifier (default: perigee-host-node)
  --config-dir <DIR>    Directory holding identity.key and trusted_clients
                        (default: PERIGEE_CONFIG_DIR, else ~/.config/perigee)
  -h, --help            Print this help text and exit
  -V, --version         Print version information and exit

Environment variables (overridden by CLI flags):
  PERIGEE_BIND_ADDR       UDP socket bind address (default: 127.0.0.1:4433)
  PERIGEE_BIND_INTERFACE  Network/VPN interface name to bind exclusively (fails closed)
  PERIGEE_CONFIG_DIR      Config directory (identity.key, trusted_clients)
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
        }
    }
}

impl HostOptions {
    /// Builds [`HostOptions`] starting from [`HostOptions::default`] and applying
    /// `PERIGEE_BIND_ADDR` and `PERIGEE_BIND_INTERFACE` from `get`.
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
        Ok(opts)
    }

    /// Builds [`HostOptions`] from process environment variables.
    pub fn from_env() -> Result<Self> {
        Self::from_env_with(|k| std::env::var(k).ok())
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

/// Parses `perigee-host` CLI arguments (`args` excludes the program name) on top of `base`.
pub fn parse_host_args(args: &[String], base: HostOptions) -> Result<HostCommand> {
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
    let daemon = Arc::new(PerigeeHostDaemon::new(options.node_name.clone(), backend));
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
                                        error = %e,
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
    use perigee_net::{ClientAuth, QuicClientTransport, QuicTransportConfig};
    use perigee_protocol::{HandshakeMessage, VideoCodec, WireMessage};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn test_run_host_stops_sessions_and_closes_connections_on_shutdown() {
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
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
            .connect(addr, "perigee.host")
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
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
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
}
