//! `rferram-server` Elevated Host Daemon Entrypoint
//!
//! Environment configuration:
//! - `RFERRAM_BIND_ADDR`: UDP socket bind address (defaults to `127.0.0.1:4433`, loopback-only).
//! - `RFERRAM_BIND_INTERFACE`: Optional VPN/network interface name to bind exclusively (fails closed if missing).
//! - `RFERRAM_SMOKE_TEST`: If set, exits cleanly immediately after binding the QUIC listener.

use anyhow::Result;
use rferram_net::{QuicServerListener, QuicTransportConfig, VpnInterfaceBinding};
use rferram_server::{RFerramHostDaemon, run_host, shutdown_signal};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let backend = RFerramHostDaemon::auto_detect_backend();
    let daemon = Arc::new(RFerramHostDaemon::new(
        "rferram-host-node".to_string(),
        backend,
    ));

    let bind_addr: SocketAddr = std::env::var("RFERRAM_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:4433".to_string())
        .parse()?;

    let transport_config = match std::env::var("RFERRAM_BIND_INTERFACE") {
        Ok(name) if !name.is_empty() => QuicTransportConfig {
            interface_binding: VpnInterfaceBinding::InterfaceName(name),
            ..Default::default()
        },
        _ => QuicTransportConfig::default(),
    };

    let listener = QuicServerListener::bind(bind_addr, transport_config)?;
    info!(
        local_addr = ?listener.local_addr()?,
        backend = ?daemon.backend,
        "rFerram Host Daemon listening for incoming QUIC connections"
    );

    if std::env::var("RFERRAM_SMOKE_TEST").is_ok() {
        return Ok(());
    }

    run_host(daemon, listener, shutdown_signal()).await
}
