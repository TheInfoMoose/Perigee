//! Quinn QUIC Endpoints, TLS 1.3 Handshake & Peer Certificate Channel Verifier
//!
//! Implements [`QuicClientTransport`], [`QuicServerListener`], and [`PeerCertChannelVerifier`]
//! enforcing TLS 1.3 with the `aws-lc-rs` crypto provider, `perigee-v1` ALPN negotiation, and SHA-256
//! leaf certificate fingerprint capture for `NistVault` Ed25519 channel binding.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use quinn::{Endpoint, EndpointConfig, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

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

    /// Establishes an outbound QUIC connection over a specified VPN socket
    pub async fn connect(
        &self,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<QuicConnectionManager> {
        info!(
            "Initiating QUIC connection to {} ({})",
            server_name, server_addr
        );

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

        let verifier = Arc::new(PeerCertChannelVerifier::new());

        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

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

        let connecting = endpoint.connect(server_addr, server_name)?;
        let connection = connecting
            .await
            .context("QUIC connection handshake failed")?;

        let peer_cert_hash = verifier.captured_fingerprint();
        info!(
            "QUIC connection successfully established with {}",
            server_name
        );
        Ok(QuicConnectionManager::new(
            connection,
            self.config.clone(),
            peer_cert_hash,
        ))
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

/// Host Daemon Transport Listener for accepting incoming remote viewer clients
pub struct QuicServerListener {
    pub config: QuicTransportConfig,
    pub endpoint: Endpoint,
    pub server_cert_sha256: [u8; 32],
}

impl QuicServerListener {
    /// Binds a QUIC server endpoint to a host network interface
    pub fn bind(bind_addr: SocketAddr, config: QuicTransportConfig) -> Result<Self> {
        info!("Binding QUIC host listener to {}", bind_addr);

        let std_socket =
            VpnSocketFactory::create_bound_udp_socket(bind_addr, &config.interface_binding)?;

        let cert = rcgen::generate_simple_self_signed(vec!["perigee.host".to_string()])?;
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.signing_key.serialize_der();

        let mut hasher = Sha256::new();
        hasher.update(&cert_der);
        let server_cert_sha256: [u8; 32] = hasher.finalize().into();

        let cert_chain = vec![CertificateDer::from(cert_der)];
        let priv_key =
            PrivateKeyDer::try_from(key_der).map_err(|e| anyhow!("Invalid private key: {e}"))?;

        let mut server_crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(cert_chain, priv_key)?;

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
        })
    }

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

/// Peer Certificate Verifier that captures the peer certificate SHA-256 digest for
/// cryptographic channel binding with `NistVault` Ed25519 signatures.
#[derive(Debug)]
pub struct PeerCertChannelVerifier {
    fingerprint: StdMutex<Option<[u8; 32]>>,
}

impl Default for PeerCertChannelVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerCertChannelVerifier {
    pub fn new() -> Self {
        Self {
            fingerprint: StdMutex::new(None),
        }
    }

    pub fn captured_fingerprint(&self) -> Option<[u8; 32]> {
        *self.fingerprint.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl rustls::client::danger::ServerCertVerifier for PeerCertChannelVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let mut hasher = Sha256::new();
        hasher.update(end_entity.as_ref());
        let digest: [u8; 32] = hasher.finalize().into();
        *self.fingerprint.lock().unwrap_or_else(|p| p.into_inner()) = Some(digest);
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}
