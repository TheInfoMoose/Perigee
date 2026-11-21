//! Quinn QUIC endpoints with mutual TLS 1.3.
//!
//! [`QuicServerListener::bind_mtls`] and [`QuicClientTransport::connect_mtls`] run TLS 1.3 with the
//! `aws-lc-rs` provider and `perigee-v1` ALPN. Each side presents a certificate made from its
//! Ed25519 node key: the client accepts only the pinned host key, and the host accepts only
//! client keys its [`PeerAuthorizer`] trusts. There is no unauthenticated transport.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use quinn::{Endpoint, EndpointConfig, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, ServerName};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::{
    AllowList, NODE_KEY_LEN, NodeIdentity, PERIGEE_SERVER_NAME, PeerAuthorizer,
    QuicConnectionManager, QuicTransportConfig, VpnSocketFactory, node_key_from_certificate,
};

/// Application-Layer Protocol Negotiation (ALPN) token for Perigee
pub const PERIGEE_ALPN: &[&[u8]] = &[b"perigee-v1"];

/// What a host needs for mutual TLS: its own node identity and who may connect.
#[derive(Debug, Clone)]
pub struct ServerAuth {
    pub identity: Arc<NodeIdentity>,
    pub authorizer: Arc<dyn PeerAuthorizer>,
}

/// What a client needs for mutual TLS: its own node identity and the host key it pins.
#[derive(Debug, Clone)]
pub struct ClientAuth {
    pub identity: Arc<NodeIdentity>,
    pub host_key: [u8; NODE_KEY_LEN],
}

/// A host identity and a client identity that trust each other, for loopback tests.
#[derive(Debug, Clone)]
pub struct LoopbackPeers {
    pub host: Arc<NodeIdentity>,
    pub client: Arc<NodeIdentity>,
}

impl LoopbackPeers {
    /// Generates two fresh node identities.
    pub fn generate() -> Result<Self> {
        Ok(Self {
            host: Arc::new(NodeIdentity::generate()?),
            client: Arc::new(NodeIdentity::generate()?),
        })
    }

    /// Host side: presents `host`, trusts only `client`.
    pub fn server_auth(&self) -> ServerAuth {
        ServerAuth {
            identity: self.host.clone(),
            authorizer: Arc::new(AllowList::new([self.client.public_key()])),
        }
    }

    /// Client side: presents `client`, pins `host`.
    pub fn client_auth(&self) -> ClientAuth {
        ClientAuth {
            identity: self.client.clone(),
            host_key: self.host.public_key(),
        }
    }
}

/// Client Transport Initiator for connecting to Remote Hosts
pub struct QuicClientTransport {
    pub config: QuicTransportConfig,
}

impl QuicClientTransport {
    pub fn new(config: QuicTransportConfig) -> Self {
        Self { config }
    }

    /// Connects to a host over mutual TLS: presents `auth.identity`'s certificate and accepts
    /// the host only if its certificate carries `auth.host_key`.
    pub async fn connect_mtls(
        &self,
        server_addr: SocketAddr,
        auth: ClientAuth,
    ) -> Result<QuicConnectionManager> {
        info!("Initiating mutual-TLS QUIC connection to {}", server_addr);

        let local_addr: SocketAddr = match server_addr {
            SocketAddr::V4(_) => "0.0.0.0:0".parse()?,
            SocketAddr::V6(_) => "[::]:0".parse()?,
        };
        let std_socket =
            VpnSocketFactory::create_bound_udp_socket(local_addr, &self.config.interface_binding)?;

        let mut transport_config = TransportConfig::default();
        transport_config.max_idle_timeout(Some(
            Duration::from_millis(self.config.max_idle_timeout_ms).try_into()?,
        ));
        transport_config.keep_alive_interval(Some(Duration::from_millis(
            self.config.keep_alive_interval_ms,
        )));
        transport_config
            .max_concurrent_bidi_streams(self.config.max_concurrent_bidi_streams.into());
        transport_config.max_concurrent_uni_streams(self.config.max_concurrent_uni_streams.into());

        let (cert, key) = auth.identity.tls_certificate()?;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let verifier = Arc::new(PinnedHostVerifier {
            host_key: auth.host_key,
            provider: provider.clone(),
        });
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![cert], key)?;
        crypto.alpn_protocols = PERIGEE_ALPN.iter().map(|s| s.to_vec()).collect();

        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
        ));
        client_config.transport_config(Arc::new(transport_config));

        let mut endpoint = Endpoint::new(
            EndpointConfig::default(),
            None,
            std_socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        endpoint.set_default_client_config(client_config);

        let connection = endpoint
            .connect(server_addr, PERIGEE_SERVER_NAME)?
            .await
            .context("mutual-TLS QUIC handshake failed")?;
        let peer_cert_sha256 = peer_certificate(&connection).map(|cert| sha256_of(&cert));
        info!(
            "Mutual-TLS QUIC connection established with {}",
            server_addr
        );
        Ok(
            QuicConnectionManager::new(connection, self.config.clone(), peer_cert_sha256)
                .with_peer_node_key(Some(auth.host_key)),
        )
    }
}

/// Most QUIC connections the host keeps open at once; further handshakes are silently dropped.
pub const MAX_OPEN_CONNECTIONS: usize = 64;

/// Most handshake attempts one source IP may start per [`HANDSHAKE_WINDOW`]; further attempts in
/// the window are silently dropped, so a peer that keeps failing the trust check is throttled.
pub const MAX_HANDSHAKES_PER_WINDOW: u32 = 20;

/// Length of the per-IP handshake counting window.
pub const HANDSHAKE_WINDOW: Duration = Duration::from_secs(60);

/// Tracked source IPs above which expired windows are pruned.
const HANDSHAKE_TRACK_PRUNE_AT: usize = 1024;

/// Tracked source IPs above which new sources are dropped until windows expire.
const HANDSHAKE_TRACK_MAX: usize = 4096;

/// Counts handshake attempts per source IP in fixed windows.
#[derive(Debug, Default)]
pub struct HandshakeLimiter {
    attempts: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl HandshakeLimiter {
    /// Records an attempt from `ip` at `now`; returns whether it is within the limit.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut attempts = self.attempts.lock().unwrap_or_else(|p| p.into_inner());
        if attempts.len() >= HANDSHAKE_TRACK_PRUNE_AT {
            attempts.retain(|_, (_, start)| now.duration_since(*start) < HANDSHAKE_WINDOW);
        }
        if attempts.len() >= HANDSHAKE_TRACK_MAX && !attempts.contains_key(&ip) {
            return false;
        }
        let entry = attempts.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= HANDSHAKE_WINDOW {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0 <= MAX_HANDSHAKES_PER_WINDOW
    }
}

/// Host Daemon Transport Listener for accepting incoming remote viewer clients
pub struct QuicServerListener {
    pub config: QuicTransportConfig,
    pub endpoint: Endpoint,
    pub server_cert_sha256: [u8; 32],
    limiter: HandshakeLimiter,
}

impl QuicServerListener {
    /// Binds a QUIC host endpoint that presents `auth.identity`'s certificate and requires every
    /// client to present a certificate whose node key `auth.authorizer` trusts.
    pub fn bind_mtls(
        bind_addr: SocketAddr,
        config: QuicTransportConfig,
        auth: ServerAuth,
    ) -> Result<Self> {
        info!("Binding mutual-TLS QUIC host listener to {}", bind_addr);

        let std_socket =
            VpnSocketFactory::create_bound_udp_socket(bind_addr, &config.interface_binding)?;

        let (cert, key) = auth.identity.tls_certificate()?;
        let server_cert_sha256 = sha256_of(&cert);

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let verifier = Arc::new(TrustedClientVerifier {
            authorizer: auth.authorizer,
            provider: provider.clone(),
        });
        let mut server_crypto = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![cert], key)?;
        server_crypto.alpn_protocols = PERIGEE_ALPN.iter().map(|s| s.to_vec()).collect();

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
        ));

        let mut transport_config = TransportConfig::default();
        transport_config.max_idle_timeout(Some(
            Duration::from_millis(config.max_idle_timeout_ms).try_into()?,
        ));
        transport_config
            .keep_alive_interval(Some(Duration::from_millis(config.keep_alive_interval_ms)));
        server_config.transport_config(Arc::new(transport_config));

        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            std_socket,
            Arc::new(quinn::TokioRuntime),
        )?;

        Ok(Self {
            config,
            endpoint,
            server_cert_sha256,
            limiter: HandshakeLimiter::default(),
        })
    }

    /// Returns the actual bound local socket address (useful when binding to port 0 in tests).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Waits for the next successfully handshaken connection. Failed handshakes are logged and skipped. Returns `None` only after the endpoint is closed.
    pub async fn accept(&self) -> Option<QuicConnectionManager> {
        while let Some(incoming) = self.endpoint.accept().await {
            let remote_addr = incoming.remote_address();
            if self.endpoint.open_connections() >= MAX_OPEN_CONNECTIONS {
                debug!("Dropping QUIC handshake from {remote_addr}: connection limit reached");
                incoming.ignore();
                continue;
            }
            if !self.limiter.allow(remote_addr.ip(), Instant::now()) {
                debug!("Dropping QUIC handshake from {remote_addr}: too many attempts");
                incoming.ignore();
                continue;
            }
            match incoming.await {
                Ok(connection) => {
                    info!(
                        "Accepted incoming QUIC connection from {}",
                        connection.remote_address()
                    );
                    let peer_node_key = peer_certificate(&connection)
                        .and_then(|cert| node_key_from_certificate(&cert).ok());
                    return Some(
                        QuicConnectionManager::new(
                            connection,
                            self.config.clone(),
                            Some(self.server_cert_sha256),
                        )
                        .with_peer_node_key(peer_node_key),
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to complete incoming QUIC handshake from {}: {e}",
                        remote_addr
                    );
                }
            }
        }
        None
    }
}

/// The leaf certificate the peer presented during the TLS handshake, if any.
fn peer_certificate(connection: &quinn::Connection) -> Option<CertificateDer<'static>> {
    let identity = connection.peer_identity()?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.first().cloned()
}

fn sha256_of(cert: &CertificateDer<'_>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    hasher.finalize().into()
}

fn not_trusted() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)
}

/// Client-side verifier: accepts the host only if its certificate carries the pinned node key.
/// The CA chain is ignored on purpose; rustls still checks the handshake signature against the
/// certificate's key, so the host proves it holds the pinned key.
#[derive(Debug)]
struct PinnedHostVerifier {
    host_key: [u8; NODE_KEY_LEN],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedHostVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if node_key_from_certificate(end_entity)? == self.host_key {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(not_trusted())
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Host-side verifier: requires a client certificate and accepts it only if its node key is
/// trusted by the authorizer.
#[derive(Debug)]
struct TrustedClientVerifier {
    authorizer: Arc<dyn PeerAuthorizer>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::server::danger::ClientCertVerifier for TrustedClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        if self
            .authorizer
            .is_authorized(&node_key_from_certificate(end_entity)?)
        {
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        } else {
            Err(not_trusted())
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handshake_limiter_throttles_per_ip_and_resets() {
        let limiter = HandshakeLimiter::default();
        let a: IpAddr = "192.0.2.1".parse().unwrap();
        let b: IpAddr = "192.0.2.2".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..MAX_HANDSHAKES_PER_WINDOW {
            assert!(limiter.allow(a, t0));
        }
        assert!(!limiter.allow(a, t0));
        assert!(limiter.allow(b, t0));
        assert!(limiter.allow(a, t0 + HANDSHAKE_WINDOW));
    }
}
