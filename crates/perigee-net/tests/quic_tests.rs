//! Integration tests for `perigee-net` QUIC endpoints, TLS 1.3 `perigee-v1` ALPN negotiation,
//! and peer certificate SHA-256 fingerprint capture for `NistVault` Ed25519 channel binding.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use perigee_common::NistVault;
use perigee_net::{
    PERIGEE_ALPN, PeerCertChannelVerifier, QuicClientTransport, QuicServerListener,
    QuicTransportConfig, VpnInterfaceBinding, VpnSocketFactory,
};
use quinn::{Endpoint, EndpointConfig};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

#[test]
fn test_perigee_alpn_constant() {
    assert_eq!(PERIGEE_ALPN, &[b"perigee-v1"]);
}

#[test]
fn test_peer_cert_channel_verifier_direct_sha256_capture() {
    let verifier = PeerCertChannelVerifier::default();
    assert_eq!(verifier.captured_fingerprint(), None);

    let generated = rcgen::generate_simple_self_signed(vec!["perigee.host".to_string()])
        .expect("Failed to generate self-signed cert");
    let cert_der_bytes = generated.cert.der().to_vec();

    let mut hasher = Sha256::new();
    hasher.update(&cert_der_bytes);
    let expected_sha256: [u8; 32] = hasher.finalize().into();

    let cert_der = CertificateDer::from(cert_der_bytes);
    let server_name = ServerName::try_from("perigee.host").expect("Valid DNS name");

    verifier
        .verify_server_cert(&cert_der, &[], &server_name, &[], UnixTime::now())
        .expect("PeerCertChannelVerifier should accept self-signed leaf certificate");

    assert_eq!(verifier.captured_fingerprint(), Some(expected_sha256));
    assert_ne!(expected_sha256, [0u8; 32]);
}

#[tokio::test]
async fn test_tls13_alpn_perigee_v1_negotiation_and_cert_sha256_capture() {
    let server_config = QuicTransportConfig::default();
    let listener = QuicServerListener::bind("127.0.0.1:0".parse().unwrap(), server_config)
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
        .connect(server_addr, "perigee.host")
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

    // 4. Verify end-to-end Ed25519 TLS 1.3 channel binding using the captured SHA-256 fingerprint
    let vault = NistVault::new("test.perigee.net");
    let (signing_key, verifying_key) = vault
        .get_or_create_ed25519_identity("quic_channel_binding_peer")
        .expect("Failed to create Ed25519 identity via NistVault");
    let public_key_bytes = verifying_key.to_bytes();

    let challenge = [0x42u8; 32];
    let signature =
        NistVault::sign_channel_bound_challenge(&signing_key, &challenge, &captured_peer_hash);
    assert_eq!(signature.len(), 64);

    NistVault::verify_channel_bound_challenge(
        &public_key_bytes,
        &challenge,
        &server_conn.peer_cert_sha256().unwrap(),
        &signature,
    )
    .expect("Channel-bound Ed25519 signature must verify with matching TLS cert SHA-256");

    // A MITM connection with a different TLS certificate SHA-256 digest must fail verification
    let mut mitm_cert_hash = captured_peer_hash;
    mitm_cert_hash[0] ^= 0xFF;
    assert!(
        NistVault::verify_channel_bound_challenge(
            &public_key_bytes,
            &challenge,
            &mitm_cert_hash,
            &signature,
        )
        .is_err(),
        "Channel-bound verification must reject mismatched TLS certificate fingerprint"
    );
}

#[tokio::test]
async fn test_alpn_mismatch_rejected_then_valid_client_accepted() {
    let listener = QuicServerListener::bind(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
    )
    .expect("Failed to bind QuicServerListener");
    let server_addr = listener.local_addr().expect("Failed to read bound addr");

    let server_task = tokio::spawn(async move { listener.accept().await });

    // Construct a raw QUIC client advertising an incompatible ALPN protocol
    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let std_socket = VpnSocketFactory::create_bound_udp_socket(
        local_addr,
        &VpnInterfaceBinding::DefaultSystemRoute,
    )
    .expect("Failed to bind client UDP socket");

    let verifier = Arc::new(PeerCertChannelVerifier::new());
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(verifier)
    .with_no_client_auth();

    crypto.alpn_protocols = vec![b"incompatible-alpn-v0".to_vec()];

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

    let connecting = endpoint
        .connect(server_addr, "perigee.host")
        .expect("Failed to initiate connection");

    let client_result = connecting.await;
    assert!(
        client_result.is_err(),
        "QUIC TLS 1.3 handshake must fail when client ALPN does not match perigee-v1"
    );

    let valid_client = QuicClientTransport::new(QuicTransportConfig::default())
        .connect(server_addr, "perigee.host")
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
    let listener = QuicServerListener::bind(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
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

    let listener = QuicServerListener::bind(
        "127.0.0.1:0".parse().unwrap(),
        QuicTransportConfig::default(),
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
        .connect(server_addr, "perigee.host")
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
