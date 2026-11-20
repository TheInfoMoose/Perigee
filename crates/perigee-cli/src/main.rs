//! `perigee` Command-Line Client and Embedded Host Entrypoint.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use perigee_cli::{CliCommand, ConnectArgs, USAGE, parse_cli_args};
use perigee_client::{ConnectOptions, SessionState, connect};
use perigee_host::{HOST_USAGE, HostCommand, HostOptions, bind_host, run_host, shutdown_signal};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_cli_args(&args, HostOptions::from_env()?)? {
        CliCommand::Help => {
            println!("{USAGE}");
            Ok(())
        }
        CliCommand::Host(HostCommand::Help) => {
            println!("{HOST_USAGE}");
            Ok(())
        }
        CliCommand::Version | CliCommand::Host(HostCommand::Version) => {
            println!("perigee {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliCommand::Host(HostCommand::Run(opts)) => {
            let (daemon, listener) = bind_host(&opts)?;

            if std::env::var("PERIGEE_SMOKE_TEST").is_ok() {
                return Ok(());
            }

            run_host(daemon, listener, shutdown_signal()).await
        }
        CliCommand::Connect(args) => run_connect(args).await,
    }
}

fn format_cert_sha256(digest: Option<[u8; 32]>) -> String {
    let Some(bytes) = digest else {
        return String::from("unknown");
    };
    let mut hex = String::with_capacity(64);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

async fn run_connect(args: ConnectArgs) -> Result<()> {
    let mut opts = ConnectOptions::new(args.target.clone());
    opts.interface = args.interface;
    opts.handshake_timeout = args.handshake_timeout;

    let mut session = connect(opts).await?;
    let cert_hex = format_cert_sha256(session.peer_cert_sha256());
    println!(
        "connected to {}; host certificate sha256 {}",
        session.target_addr, cert_hex
    );

    let frame_count = Arc::new(AtomicU64::new(0));
    if let Some(mut video_rx) = session.take_video_rx() {
        let frame_count_task = frame_count.clone();
        tokio::spawn(async move {
            while video_rx.recv().await.is_some() {
                frame_count_task.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let run_for_deadline = args.run_for.map(|d| tokio::time::Instant::now() + d);

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick so status prints every 1 s.
    tick.tick().await;

    let mut state_rx = session.state_rx.clone();

    let host_terminated: Option<String> = loop {
        if let SessionState::Terminated { reason } = state_rx.borrow_and_update().clone() {
            break Some(reason);
        }

        let sleep_until_for = async {
            match run_for_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            biased;
            _ = &mut shutdown => break None,
            _ = sleep_until_for => break None,
            changed = state_rx.changed() => {
                if changed.is_err() {
                    break Some(String::from("session state channel closed"));
                }
                if let SessionState::Terminated { reason } = state_rx.borrow_and_update().clone() {
                    break Some(reason);
                }
            }
            _ = tick.tick() => {
                let state = session.state();
                let frames = frame_count.load(Ordering::Relaxed);
                let (current_fps, rtt_ms) = {
                    let m = session.bridge.pipeline.metrics.lock().await;
                    (m.current_fps, m.rtt_ms)
                };
                println!(
                    "state={state:?} frames={frames} fps={current_fps:.1} rtt_ms={rtt_ms:.1}"
                );
            }
        }
    };

    if let Some(reason) = host_terminated {
        session.disconnect("host ended session").await;
        println!("{reason}");
        std::process::exit(1);
    }

    session.disconnect("user exit").await;
    println!("disconnected");
    Ok(())
}
