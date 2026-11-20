//! Integration tests for `perigee-net` QUIC endpoints: mutual TLS with pinned Ed25519 node keys,
//! TLS 1.3 `perigee-v1` ALPN negotiation, datagrams and length-prefixed streams.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use perigee_net::{
    AllowList, ClientAuth, LoopbackPeers, PERIGEE_ALPN, QuicClientTransport, QuicServerListener,
    QuicTransportConfig, ServerAuth, VpnInterfaceBinding, VpnSocketFactory,
};
use perigee_protocol::{ControlMessage, WireMessage};
use quinn::{Endpoint, EndpointConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// Test-only verifier for raw clients that must reach the host's handshake (bad ALPN, no
/// client certificate). It accepts any host certificate; production clients pin the host key.
#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Opens a raw QUIC connection that offers `alpn` and presents no client certificate.
async fn connect_raw_without_client_cert(
    server_addr: SocketAddr,
    alpn: &[u8],
) -> Result<quinn::Connection, quinn::ConnectionError> {
    let std_socket = VpnSocketFactory::create_bound_udp_socket(
        "127.0.0.1:0".parse().unwrap(),
        &VpnInterfaceBinding::DefaultSystemRoute,
    )
    .expect("Failed to bind client UDP socket");
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
    .with_no_client_auth();
    crypto.alpn_protocols = vec![alpn.to_vec()];
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    let mut endpoint = Endpoint::new(
        EndpointConfig::default(),
        None,
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )
    .expect("Failed to create client endpoint");
    endpoint.set_default_client_config(client_config);
    endpoint
        .connect(server_addr, "perigee.host")
        .expect("Failed to initiate connection")
        .await
}

#[test]
fn test_perigee_alpn_constant() {
    assert_eq!(PERIGEE_ALPN, &[b"perigee-v1"]);
}

#[tokio::test]
async fn test_tls13_alpn_perigee_v1_negotiation_and_cert_sha256_capture() {
    let peers = LoopbackPeers::generate().unwrap();
    let server_config = QuicTransportConfig::default();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        server_config,
        peers.server_auth(),
    )
    .expect("Failed to bind QuicServerListener");
    let server_addr = listener.local_addr().expect("Failed to read bound addr");
    let expected_server_cert_hash = listener.server_cert_sha256;
    assert_ne!(
        expected_server_cert_hash, [0u8; 32],
        "Server certificate SHA-256 fingerprint must be non-zero"
    );

    let server_task = tokio::spawn(async move {
        listener
            .accept()
            .await
            .expect("Server failed to accept incoming QUIC connection")
    });

    let client = QuicClientTransport::new(QuicTransportConfig::default());
    let client_conn = client
        .connect_mtls(server_addr, peers.client_auth())
        .await
        .expect("Client failed to complete QUIC TLS 1.3 handshake");

    let server_conn = server_task.await.expect("Server accept task panicked");

    // 1. Verify TLS 1.3 ALPN `perigee-v1` negotiated on client connection
    let client_handshake = client_conn
        .connection
        .handshake_data()
        .expect("Client handshake data should be present")
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .expect("Handshake data should downcast to rustls HandshakeData");
    assert_eq!(
        client_handshake.protocol.as_deref(),
        Some(b"perigee-v1".as_slice()),
        "Client must negotiate ALPN perigee-v1"
    );

    // 2. Verify TLS 1.3 ALPN `perigee-v1` and SNI negotiated on server connection
    let server_handshake = server_conn
        .connection
        .handshake_data()
        .expect("Server handshake data should be present")
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .expect("Server handshake data should downcast to rustls HandshakeData");
    assert_eq!(
        server_handshake.protocol.as_deref(),
        Some(b"perigee-v1".as_slice()),
        "Server must negotiate ALPN perigee-v1"
    );
    assert_eq!(
        server_handshake.server_name.as_deref(),
        Some("perigee.host")
    );

    // 3. Verify peer certificate SHA-256 fingerprint capture matches server leaf certificate
    let captured_peer_hash = client_conn
        .peer_cert_sha256()
        .expect("Client connection must capture peer TLS leaf cert SHA-256 fingerprint");
    assert_eq!(captured_peer_hash, expected_server_cert_hash);
    assert_eq!(
        server_conn.peer_cert_sha256(),
        Some(expected_server_cert_hash)
    );
}

#[tokio::test]
async fn test_alpn_mismatch_rejected_then_valid_client_accepted() {
    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .expect("Failed to bind QuicServerListener");
    let server_addr = listener.local_addr().expect("Failed to read bound addr");

    let server_task = tokio::spawn(async move { listener.accept().await });

    // A raw QUIC client advertising an incompatible ALPN protocol
    let client_result = connect_raw_without_client_cert(server_addr, b"incompatible-alpn-v0").await;
    assert!(
        client_result.is_err(),
        "QUIC TLS 1.3 handshake must fail when client ALPN does not match perigee-v1"
    );

    let valid_client = QuicClientTransport::new(QuicTransportConfig::default())
        .connect_mtls(server_addr, peers.client_auth())
        .await;
    assert!(
        valid_client.is_ok(),
        "Valid client must succeed after a failed handshake"
    );

    let server_accepted = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("Server accept timed out")
        .expect("Server task panicked");
    assert!(
        server_accepted.is_some(),
        "Server must accept valid connection after skipping mismatched ALPN"
    );
}

#[tokio::test]
async fn test_accept_returns_none_after_endpoint_close() {
    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .expect("Failed to bind QuicServerListener");

    listener.endpoint.close(0u32.into(), b"test");

    let accepted = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accept() timed out after endpoint close");
    assert!(
        accepted.is_none(),
        "accept() must return None after endpoint is closed"
    );
}

#[tokio::test]
async fn test_loopback_datagram_and_length_prefixed_uni_stream_transmission() {
    use perigee_net::NetworkMetrics;
    use perigee_protocol::{
        ControlMessage, ElementState, InputEvent, MouseButtonCode, PixelFormat, VideoCodec,
        VideoFrameMessage, WireMessage,
    };
    use std::sync::atomic::Ordering;

    assert_eq!(NetworkMetrics::calculate_loss_rate(200, 10), 5.0);

    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .expect("Failed to bind QuicServerListener");
    let server_addr = listener.local_addr().expect("Failed to read bound addr");

    let expected_dgram = WireMessage::Input(InputEvent::MouseButton {
        button: MouseButtonCode::Right,
        state: ElementState::Pressed,
        x: 1024.0,
        y: 768.0,
    });
    let expected_uni_video = WireMessage::Video(VideoFrameMessage {
        frame_id: 7,
        monitor_id: 1,
        timestamp_us: 33_333,
        codec: VideoCodec::AV1,
        is_keyframe: true,
        width: 2560,
        height: 1440,
        stride: 2560 * 4,
        format: PixelFormat::Bgra8,
        payload: vec![0xCD; 2048],
    });
    let expected_ack_dgram = WireMessage::Control(ControlMessage::FrameAck {
        frame_id: 7,
        monitor_id: 1,
        render_delay_us: 310,
    });

    let server_video = expected_uni_video.clone();
    let server_task = tokio::spawn(async move {
        let server_conn = listener
            .accept()
            .await
            .expect("Server failed to accept incoming QUIC connection");

        // 1. Server receives client input datagram
        let recv_input = server_conn
            .recv_datagram()
            .await
            .expect("Server failed to receive input datagram");

        // 2. Server sends length-prefixed VideoFrameMessage over unidirectional stream
        server_conn
            .send_uni_message(&server_video)
            .await
            .expect("Server failed to send length-prefixed unidirectional stream");

        // 3. Server receives FrameAck datagram from client
        let recv_ack = server_conn
            .recv_datagram()
            .await
            .expect("Server failed to receive FrameAck datagram");

        assert_eq!(
            server_conn
                .metrics
                .datagrams_received
                .load(Ordering::Relaxed),
            2
        );
        assert!(server_conn.metrics.bytes_sent.load(Ordering::Relaxed) > 2048);

        (recv_input, recv_ack)
    });

    let client = QuicClientTransport::new(QuicTransportConfig::default());
    let client_conn = client
        .connect_mtls(server_addr, peers.client_auth())
        .await
        .expect("Client failed to connect");

    // 1. Client sends input datagram
    client_conn
        .send_datagram(&expected_dgram)
        .expect("Client failed to send input datagram");

    // 2. Client receives length-prefixed unidirectional stream video frame
    let client_recv_video = client_conn
        .recv_uni_message()
        .await
        .expect("Client failed to receive length-prefixed unidirectional stream");
    assert_eq!(client_recv_video, expected_uni_video);

    // 3. Client sends FrameAck datagram
    client_conn
        .send_datagram(&expected_ack_dgram)
        .expect("Client failed to send FrameAck datagram");

    let (srv_input, srv_ack) = server_task.await.expect("Server task panicked");
    assert_eq!(srv_input, expected_dgram);
    assert_eq!(srv_ack, expected_ack_dgram);
    assert_eq!(
        client_conn.metrics.datagrams_sent.load(Ordering::Relaxed),
        2
    );
    assert!(client_conn.metrics.bytes_received.load(Ordering::Relaxed) > 2048);
}

#[tokio::test]
async fn test_mtls_trusted_client_connects_and_sees_keys() {
    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let conn = listener
            .accept()
            .await
            .expect("trusted client should be accepted");
        let msg = conn
            .recv_uni_message()
            .await
            .expect("message from trusted client");
        (listener, conn.peer_node_key(), msg)
    });

    let client_conn = QuicClientTransport::new(QuicTransportConfig::default())
        .connect_mtls(addr, peers.client_auth())
        .await
        .expect("mutual-TLS connect should succeed");
    assert_eq!(client_conn.peer_node_key(), Some(peers.host.public_key()));

    let probe = WireMessage::Control(ControlMessage::Disconnect {
        reason: "mtls-probe".to_string(),
    });
    client_conn.send_uni_message(&probe).await.unwrap();

    let (_listener, seen_client_key, received) =
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("server task timed out")
            .expect("server task panicked");
    assert_eq!(seen_client_key, Some(peers.client.public_key()));
    assert_eq!(received, probe);
}

#[tokio::test]
async fn test_mtls_rejects_untrusted_client() {
    let peers = LoopbackPeers::generate().unwrap();
    let server_auth = ServerAuth {
        identity: peers.host.clone(),
        authorizer: Arc::new(AllowList::default()),
    };
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        server_auth,
    )
    .unwrap();
    let addr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .is_err()
    });

    let attempt = QuicClientTransport::new(QuicTransportConfig::default())
        .connect_mtls(addr, peers.client_auth())
        .await;
    if let Ok(conn) = attempt {
        tokio::time::timeout(Duration::from_secs(5), conn.connection.closed())
            .await
            .expect("host must close an untrusted client's connection");
    }
    assert!(
        server_task.await.unwrap(),
        "host must not accept an untrusted client"
    );
}

#[tokio::test]
async fn test_mtls_client_rejects_wrong_host_key() {
    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .unwrap();
    let addr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .is_err()
    });

    let wrong_pin = ClientAuth {
        identity: peers.client.clone(),
        host_key: [0u8; 32],
    };
    let result = QuicClientTransport::new(QuicTransportConfig::default())
        .connect_mtls(addr, wrong_pin)
        .await;
    assert!(
        result.is_err(),
        "client must refuse a host whose key is not the pinned one"
    );
    assert!(server_task.await.unwrap());
}

#[tokio::test]
async fn test_mtls_rejects_client_without_certificate() {
    let peers = LoopbackPeers::generate().unwrap();
    let listener = QuicServerListener::bind_mtls(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
        peers.server_auth(),
    )
    .unwrap();
    let addr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .is_err()
    });

    let attempt = connect_raw_without_client_cert(addr, b"perigee-v1").await;
    if let Ok(conn) = attempt {
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("host must close a client that sent no certificate");
    }
    assert!(
        server_task.await.unwrap(),
        "host must not accept a client without a certificate"
    );
}
