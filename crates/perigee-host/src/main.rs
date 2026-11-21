//! `perigee-host` Elevated Host Daemon Entrypoint
//!
//! Command-line flags (override environment variables):
//! - `--bind <ADDR>`: UDP socket bind address (defaults to `127.0.0.1:4433`, loopback-only).
//! - `--interface <NAME>`: Bind exclusively to network/VPN interface `NAME` (fails closed if missing).
//! - `--name <NODE>`: Host node identifier (defaults to `perigee-host-node`).
//! - `-h` / `--help`: Print usage and exit.
//! - `-V` / `--version`: Print version and exit.
//!
//! Environment configuration (overridden by CLI flags):
//! - `PERIGEE_BIND_ADDR`: UDP socket bind address (defaults to `127.0.0.1:4433`, loopback-only).
//! - `PERIGEE_BIND_INTERFACE`: Optional VPN/network interface name to bind exclusively (fails closed if missing).
//! - `PERIGEE_SMOKE_TEST`: If set, exits cleanly immediately after binding the QUIC listener.

use anyhow::Result;
use perigee_host::{
    HOST_USAGE, HostCommand, bind_host, resolve_host_command, run_host, run_host_admin,
    run_host_pair, shutdown_signal,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match resolve_host_command(&args, |k| std::env::var(k).ok())? {
        HostCommand::Help => {
            println!("{HOST_USAGE}");
            Ok(())
        }
        HostCommand::Version => {
            println!("perigee-host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        HostCommand::Admin(admin) => {
            println!("{}", run_host_admin(&admin)?);
            Ok(())
        }
        HostCommand::Pair(args) => {
            let done = run_host_pair(&args, |text| println!("{text}")).await?;
            println!("{done}");
            Ok(())
        }
        HostCommand::Run(opts) => {
            let (daemon, listener) = bind_host(&opts)?;

            if std::env::var("PERIGEE_SMOKE_TEST").is_ok() {
                return Ok(());
            }

            run_host(daemon, listener, shutdown_signal()).await
        }
    }
}
