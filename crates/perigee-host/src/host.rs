//! Host accept loop and graceful shutdown.

use crate::PerigeeHostDaemon;
use anyhow::Result;
use perigee_net::QuicServerListener;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

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
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM signal handler");
        tokio::select! {
            res = tokio::signal::ctrl_c() => {
                match res {
                    Ok(()) => info!("Received SIGINT (Ctrl-C), initiating host shutdown"),
                    Err(e) => warn!(error = %e, "Failed listening for SIGINT, initiating host shutdown"),
                }
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, initiating host shutdown");
            }
        }
    }

    #[cfg(not(unix))]
    {
        match tokio::signal::ctrl_c().await {
            Ok(()) => info!("Received SIGINT (Ctrl-C), initiating host shutdown"),
            Err(e) => warn!(error = %e, "Failed listening for SIGINT, initiating host shutdown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureBackendType;
    use perigee_common::NistVault;
    use perigee_net::{QuicClientTransport, QuicTransportConfig};
    use perigee_protocol::{AuthMethod, HandshakeMessage, VideoCodec, WireMessage};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn test_run_host_stops_sessions_and_closes_connections_on_shutdown() {
        let listener = QuicServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            QuicTransportConfig::default(),
        )
        .expect("Failed to bind QUIC listener");
        let addr = listener.local_addr().unwrap();

        let daemon = Arc::new(PerigeeHostDaemon::with_vault(
            "host-test".into(),
            CaptureBackendType::WaylandPipeWire,
            NistVault::new("test.perigee.server.host"),
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
            auth_methods: vec![AuthMethod::Ed25519Signature],
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

        let daemon = Arc::new(PerigeeHostDaemon::with_vault(
            "host-close-test".into(),
            CaptureBackendType::WaylandPipeWire,
            NistVault::new("test.perigee.server.host_close"),
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
}
