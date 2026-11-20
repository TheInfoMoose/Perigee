//! Frontend-agnostic connect flow: resolve the target, open the QUIC connection, run the session handshake, and hand back a live ClientSession.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use perigee_net::{
    ClientAuth, IDENTITY_FILE_NAME, KNOWN_HOSTS_FILE_NAME, NODE_KEY_LEN, NodeIdentity,
    QuicClientTransport, QuicTransportConfig, TrustedKeys, VpnInterfaceBinding,
};
use perigee_protocol::InputEvent;
use tokio::sync::{mpsc, watch};

use crate::bridge::QuicSessionBridge;
use crate::render::DecodedFrameSurface;
use crate::session::{SessionConfig, SessionState};

/// Default QUIC port used when a target host address omits `:port`.
pub const DEFAULT_PERIGEE_PORT: u16 = 4433;

/// Default maximum time to wait for a session handshake response after QUIC connects.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Parses or resolves a target host address string into a [`SocketAddr`].
///
/// Accepts `ip:port`, `[ipv6]:port`, bare IP addresses (defaulting to port `4433`),
/// and `hostname:port` / `hostname` strings via [`ToSocketAddrs`].
pub fn resolve_host_addr(target: &str) -> Result<SocketAddr> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        bail!("Connection target host address must not be empty");
    }

    if let Ok(addr) = trimmed.parse::<SocketAddr>() {
        return Ok(addr);
    }

    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PERIGEE_PORT));
    }

    let with_default_port = if trimmed.contains(':') {
        trimmed.to_string()
    } else {
        format!("{trimmed}:{DEFAULT_PERIGEE_PORT}")
    };

    let mut resolved = with_default_port
        .to_socket_addrs()
        .with_context(|| format!("Failed to resolve target host address '{trimmed}'"))?;
    resolved
        .next()
        .ok_or_else(|| anyhow!("No socket addresses resolved for '{trimmed}'"))
}

/// Parses an optional interface specification into a [`VpnInterfaceBinding`].
pub fn parse_interface_binding(spec: Option<&str>) -> VpnInterfaceBinding {
    let Some(raw) = spec else {
        return VpnInterfaceBinding::DefaultSystemRoute;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        VpnInterfaceBinding::DefaultSystemRoute
    } else if let Ok(ip) = trimmed.parse::<IpAddr>() {
        VpnInterfaceBinding::SpecificIp(ip)
    } else {
        VpnInterfaceBinding::InterfaceName(trimmed.to_string())
    }
}

/// Mutual-TLS credentials from a Perigee config directory: this machine's node identity
/// (`identity.key`, created on first use) and the host key for `target`, which is
/// `host_key_override` when given, else the pin saved in `known_hosts` under the label `target`.
pub fn load_client_auth(
    config_dir: &Path,
    target: &str,
    host_key_override: Option<[u8; NODE_KEY_LEN]>,
) -> Result<ClientAuth> {
    let identity = Arc::new(NodeIdentity::load_or_create(
        &config_dir.join(IDENTITY_FILE_NAME),
    )?);
    let host_key = match host_key_override {
        Some(key) => key,
        None => TrustedKeys::load(config_dir.join(KNOWN_HOSTS_FILE_NAME))?
            .find_by_label(target)
            .map(|entry| entry.key)
            .ok_or_else(|| {
                anyhow!(
                    "no pinned host key for '{target}': run `perigee trust add {target} <KEY>` with the key `perigee-host identity` prints, or pass --host-key"
                )
            })?,
    };
    Ok(ClientAuth { identity, host_key })
}

/// Options controlling how [`connect`] establishes a [`ClientSession`].
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub target: String,
    pub interface: Option<String>,
    pub handshake_timeout: Duration,
    pub session: SessionConfig,
    /// Mutual-TLS identity and pinned host key. `None` uses the legacy unauthenticated
    /// transport (removed in STEP 4O).
    pub auth: Option<ClientAuth>,
}

impl ConnectOptions {
    /// Creates default connection options for `target`.
    pub fn new(target: impl Into<String>) -> Self {
        let target = target.into();
        let session = SessionConfig {
            host_address: target.clone(),
            ..Default::default()
        };
        Self {
            target,
            interface: None,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            session,
            auth: None,
        }
    }
}

/// An established, authenticated client session with an active QUIC transport bridge.
pub struct ClientSession {
    pub target_addr: SocketAddr,
    pub interface_binding: VpnInterfaceBinding,
    pub bridge: Arc<QuicSessionBridge>,
    pub state_rx: watch::Receiver<SessionState>,
    pub input_tx: mpsc::Sender<InputEvent>,
    video_rx: Option<mpsc::Receiver<DecodedFrameSurface>>,
}

impl ClientSession {
    /// Takes ownership of the decoded video frame receiver, if not already taken.
    pub fn take_video_rx(&mut self) -> Option<mpsc::Receiver<DecodedFrameSurface>> {
        self.video_rx.take()
    }

    /// Returns the current [`SessionState`] of this session.
    pub fn state(&self) -> SessionState {
        self.state_rx.borrow().clone()
    }

    /// Returns the SHA-256 digest of the host's TLS 1.3 leaf certificate.
    pub fn peer_cert_sha256(&self) -> Option<[u8; 32]> {
        self.bridge.peer_cert_sha256()
    }

    /// Returns the host's Ed25519 node key when the session uses mutual TLS.
    pub fn peer_node_key(&self) -> Option<[u8; NODE_KEY_LEN]> {
        self.bridge.connection.peer_node_key()
    }

    /// Cleanly disconnects the session: notifies the host, closes the QUIC connection,
    /// marks the session `Terminated`, and drains spawned pipeline and bridge tasks.
    pub async fn disconnect(&self, reason: &str) {
        self.bridge.disconnect(reason).await
    }
}

/// Resolves `options.target`, connects over QUIC, runs the initial session handshake,
/// and returns a [`ClientSession`] once the host accepts the session (`SessionState::Active`).
pub async fn connect(options: ConnectOptions) -> Result<ClientSession> {
    let target_label = options.target.clone();
    let target_addr = resolve_host_addr(&options.target)
        .with_context(|| format!("Failed to resolve target '{target_label}'"))?;
    let interface_binding = parse_interface_binding(options.interface.as_deref());
    let transport_config = QuicTransportConfig {
        interface_binding: interface_binding.clone(),
        ..Default::default()
    };

    let transport = QuicClientTransport::new(transport_config);
    let conn = match options.auth.clone() {
        Some(auth) => transport.connect_mtls(target_addr, auth).await,
        None => transport.connect(target_addr, "perigee.host").await,
    }
    .with_context(|| {
        format!("Failed to establish QUIC connection to '{target_label}' ({target_addr})")
    })?;

    let (bridge, mut state_rx, video_rx, input_tx) =
        QuicSessionBridge::spawn(options.session, Arc::new(conn));
    let bridge = Arc::new(bridge);

    let timeout = options.handshake_timeout;
    let wait_result = tokio::time::timeout(timeout, async {
        loop {
            let current = state_rx.borrow_and_update().clone();
            match current {
                SessionState::Active => return Ok(()),
                SessionState::Terminated { reason } => {
                    return Err(anyhow!("host rejected the session: {reason}"));
                }
                _ => {}
            }
            if state_rx.changed().await.is_err() {
                return Err(anyhow!(
                    "session pipeline closed before completing handshake"
                ));
            }
        }
    })
    .await;

    match wait_result {
        Ok(Ok(())) => Ok(ClientSession {
            target_addr,
            interface_binding,
            bridge,
            state_rx,
            input_tx,
            video_rx: Some(video_rx),
        }),
        Ok(Err(err)) => {
            bridge.disconnect("handshake failed").await;
            Err(err).with_context(|| format!("Handshake with '{target_label}' failed"))
        }
        Err(_) => {
            bridge.disconnect("handshake failed").await;
            Err(anyhow!(
                "no handshake response from {target_addr} within {timeout:?}"
            ))
            .with_context(|| format!("Handshake with '{target_label}' timed out"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perigee_net::{LoopbackPeers, QuicServerListener};
    use perigee_protocol::{HandshakeMessage, WireMessage};

    #[test]
    fn test_resolve_host_addr() {
        let addr = resolve_host_addr("192.0.2.10:5000").unwrap();
        assert_eq!(addr, "192.0.2.10:5000".parse::<SocketAddr>().unwrap());

        let addr_default = resolve_host_addr("192.0.2.10").unwrap();
        assert_eq!(
            addr_default,
            SocketAddr::new("192.0.2.10".parse().unwrap(), DEFAULT_PERIGEE_PORT)
        );

        let v6 = resolve_host_addr("[2001:db8::1]:5000").unwrap();
        assert_eq!(v6, "[2001:db8::1]:5000".parse::<SocketAddr>().unwrap());

        assert!(resolve_host_addr("").is_err());

        let localhost = resolve_host_addr("localhost").unwrap();
        assert_eq!(localhost.port(), DEFAULT_PERIGEE_PORT);
    }

    #[test]
    fn test_parse_interface_binding() {
        assert_eq!(
            parse_interface_binding(None),
            VpnInterfaceBinding::DefaultSystemRoute
        );
        assert_eq!(
            parse_interface_binding(Some(" ")),
            VpnInterfaceBinding::DefaultSystemRoute
        );
        assert_eq!(
            parse_interface_binding(Some("192.0.2.2")),
            VpnInterfaceBinding::SpecificIp("192.0.2.2".parse().unwrap())
        );
        assert_eq!(
            parse_interface_binding(Some("wg0")),
            VpnInterfaceBinding::InterfaceName(String::from("wg0"))
        );
    }

    #[tokio::test]
    async fn test_connect_succeeds_after_auth_response() {
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();
        let expected_cert_hash = listener.server_cert_sha256;

        let server_task = tokio::spawn(async move {
            let server_conn = listener.accept().await.expect("Server failed to accept");
            let hello = server_conn
                .recv_uni_message()
                .await
                .expect("Failed to read ClientHello");
            assert!(matches!(
                hello,
                WireMessage::Handshake(HandshakeMessage::ClientHello { .. })
            ));

            server_conn
                .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                    success: true,
                    session_token: Some(String::from("tok-connect-01")),
                    error_message: None,
                    requires_elevation: false,
                }))
                .await
                .expect("Failed to send AuthResponse");

            (listener, server_conn)
        });

        let mut session = connect(ConnectOptions::new(server_addr.to_string()))
            .await
            .expect("connect should succeed");

        assert_eq!(session.state(), SessionState::Active);
        assert_eq!(session.peer_cert_sha256(), Some(expected_cert_hash));
        assert!(session.take_video_rx().is_some());
        assert!(session.take_video_rx().is_none());

        session.bridge.stop();
        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_fails_when_host_rejects() {
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let server_conn = listener.accept().await.expect("Server failed to accept");
            let _hello = server_conn
                .recv_uni_message()
                .await
                .expect("Failed to read ClientHello");

            server_conn
                .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                    success: false,
                    session_token: None,
                    error_message: Some(String::from("denied")),
                    requires_elevation: false,
                }))
                .await
                .expect("Failed to send rejection AuthResponse");

            (listener, server_conn)
        });

        let result = connect(ConnectOptions::new(server_addr.to_string())).await;
        let Err(err) = result else {
            panic!("expected connect to fail when host rejects");
        };
        let err_text = format!("{err:#}");
        assert!(
            err_text.contains("denied"),
            "expected error to contain 'denied', got: {err_text}"
        );

        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_times_out_without_response() {
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
        )
        .expect("Failed to bind QUIC listener");
        let server_addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let server_conn = listener.accept().await.expect("Server failed to accept");
            let _ = server_conn.connection.closed().await;
            (listener, server_conn)
        });

        let mut opts = ConnectOptions::new(server_addr.to_string());
        opts.handshake_timeout = Duration::from_millis(300);

        let result = tokio::time::timeout(Duration::from_secs(2), connect(opts))
            .await
            .expect("connect should time out within 2s");

        let Err(err) = result else {
            panic!("expected connect to time out");
        };
        let err_text = format!("{err:#}");
        assert!(
            err_text.contains("no handshake response"),
            "expected error to contain 'no handshake response', got: {err_text}"
        );

        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_mtls_with_pinned_host_key() {
        let peers = LoopbackPeers::generate().unwrap();
        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind mutual-TLS listener");
        let server_addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let server_conn = listener.accept().await.expect("Server failed to accept");
            let _hello = server_conn
                .recv_uni_message()
                .await
                .expect("Failed to read ClientHello");
            server_conn
                .send_uni_message(&WireMessage::Handshake(HandshakeMessage::AuthResponse {
                    success: true,
                    session_token: Some(String::from("tok-mtls-01")),
                    error_message: None,
                    requires_elevation: false,
                }))
                .await
                .expect("Failed to send AuthResponse");
            (listener, server_conn)
        });

        let mut options = ConnectOptions::new(server_addr.to_string());
        options.auth = Some(peers.client_auth());
        let session = connect(options).await.expect("mTLS connect should succeed");

        assert_eq!(session.state(), SessionState::Active);
        assert_eq!(session.peer_node_key(), Some(peers.host.public_key()));

        session.disconnect("test done").await;
        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_mtls_refuses_wrong_host_key() {
        let peers = LoopbackPeers::generate().unwrap();
        let listener = QuicServerListener::bind_mtls(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
            peers.server_auth(),
        )
        .expect("Failed to bind mutual-TLS listener");
        let server_addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await;
            listener
        });

        let mut options = ConnectOptions::new(server_addr.to_string());
        options.auth = Some(ClientAuth {
            identity: peers.client.clone(),
            host_key: [0u8; 32],
        });
        let Err(err) = connect(options).await else {
            panic!("connect must refuse a host whose key is not pinned");
        };
        assert!(
            format!("{err:#}").contains("mutual-TLS"),
            "unexpected error: {err:#}"
        );
        let _ = server_task.await.unwrap();
    }
}
