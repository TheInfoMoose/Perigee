//! `perigee` Command-Line Client and Embedded Host Entrypoint.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use perigee_cli::{
    CliCommand, ConnectArgs, USAGE, client_auth_for, resolve_cli_command, run_client_admin,
    run_client_pair,
};
use perigee_client::{
    ClientSession, ConnectOptions, SessionState, ViewerWindowConfig, connect, run_viewer_window,
};
use perigee_host::{
    HOST_USAGE, HostCommand, bind_host, run_host, run_host_admin, run_host_pair, run_host_tool,
    shutdown_signal,
};
use perigee_net::node_key_to_hex;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match resolve_cli_command(&args, |k| std::env::var(k).ok())? {
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
        CliCommand::Admin(admin) => {
            println!("{}", run_client_admin(&admin)?);
            Ok(())
        }
        CliCommand::Pair(args) => {
            println!("{}", run_client_pair(&args).await?);
            Ok(())
        }
        CliCommand::Host(HostCommand::Admin(admin)) => {
            println!("{}", run_host_admin(&admin)?);
            Ok(())
        }
        CliCommand::Host(HostCommand::Pair(args)) => {
            let done = run_host_pair(&args, |text| println!("{text}")).await?;
            println!("{done}");
            Ok(())
        }
        CliCommand::Host(HostCommand::Tool(tool)) => {
            println!("{}", run_host_tool(&tool)?);
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

async fn run_connect(args: ConnectArgs) -> Result<()> {
    let auth = client_auth_for(&args)?;
    let mut opts = ConnectOptions::new(args.target.clone(), auth);
    opts.interface = args.interface;
    opts.handshake_timeout = args.handshake_timeout;

    let mut session = connect(opts).await?;
    let host_key = session
        .peer_node_key()
        .map(|key| node_key_to_hex(&key))
        .unwrap_or_else(|| String::from("unknown"));
    println!(
        "connected to {}; host node key {}",
        session.target_addr, host_key
    );

    if args.window {
        return run_connect_window(session, args.target, args.run_for).await;
    }

    let frame_count = Arc::new(AtomicU64::new(0));
    if let Some(mut video_rx) = session.take_video_rx() {
        let frame_count_task = frame_count.clone();
        let mut dump_path = args.dump_frame.clone();
        let mut dump_error_reported = false;
        tokio::spawn(async move {
            while let Some(frame) = video_rx.recv().await {
                frame_count_task.fetch_add(1, Ordering::Relaxed);
                if let Some(path) = dump_path.clone() {
                    let written = perigee_host::encode_ppm(
                        frame.width,
                        frame.height,
                        frame.width.saturating_mul(4),
                        frame.format,
                        &frame.data,
                    )
                    .and_then(|ppm| std::fs::write(&path, ppm).map_err(anyhow::Error::from));
                    match written {
                        Ok(()) => {
                            println!(
                                "wrote {}x{} frame to {}",
                                frame.width,
                                frame.height,
                                path.display()
                            );
                            dump_path = None;
                        }
                        Err(e) if !dump_error_reported => {
                            eprintln!(
                                "could not write {} yet: {e:#} (will retry on the next frame)",
                                path.display()
                            );
                            dump_error_reported = true;
                        }
                        Err(_) => {}
                    }
                }
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

async fn run_connect_window(
    mut session: ClientSession,
    target: String,
    run_for: Option<Duration>,
) -> Result<()> {
    let video_rx = session
        .take_video_rx()
        .ok_or_else(|| anyhow!("client session video receiver is unavailable"))?;

    let (window_shutdown_tx, mut window_shutdown_rx) = tokio::sync::watch::channel(false);

    let lifecycle_task = tokio::spawn(async move {
        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);

        let run_for_deadline = run_for.map(|d| tokio::time::Instant::now() + d);

        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                changed = window_shutdown_rx.changed() => {
                    if changed.is_err() || *window_shutdown_rx.borrow() {
                        break None;
                    }
                }
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
                    let (current_fps, rtt_ms) = {
                        let m = session.bridge.pipeline.metrics.lock().await;
                        (m.current_fps, m.rtt_ms)
                    };
                    println!(
                        "state={state:?} fps={current_fps:.1} rtt_ms={rtt_ms:.1}"
                    );
                }
            }
        };

        if let Some(reason) = host_terminated {
            session.disconnect("host ended session").await;
            println!("{reason}");
            return Some(reason);
        }

        session.disconnect("user exit").await;
        println!("disconnected");
        None
    });

    let viewer_config = ViewerWindowConfig {
        title: format!("Perigee - {target}"),
        ..Default::default()
    };
    let window_result = tokio::task::block_in_place(|| {
        run_viewer_window(viewer_config, video_rx, window_shutdown_tx)
    });
    let host_terminated = lifecycle_task.await?;
    window_result?;

    if host_terminated.is_some() {
        std::process::exit(1);
    }

    Ok(())
}
