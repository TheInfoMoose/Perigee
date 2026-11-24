//! End-to-end loopback integration test between the real `perigee-host` daemon
//! and the real `perigee-client` connect API (`CLI-06`, `REPO-05`).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use perigee_cli::{ClientPairArgs, ConnectArgs, client_auth_for, run_client_pair};
use perigee_client::{ConnectOptions, SessionState, connect};
use perigee_host::{HostCodec, HostOptions, MediaMode, bind_host, run_host};
use perigee_net::{
    ClientAuth, IDENTITY_FILE_NAME, NODE_KEY_LEN, NodeIdentity, PairingWindow,
    TRUSTED_CLIENTS_FILE_NAME, TrustedKeys,
};
use perigee_protocol::{InputEvent, PixelFormat};
use tokio::sync::oneshot;

fn trusted_options(
    addr: &SocketAddr,
    client: &Arc<NodeIdentity>,
    host_key: [u8; NODE_KEY_LEN],
) -> ConnectOptions {
    ConnectOptions::new(
        addr.to_string(),
        ClientAuth {
            identity: client.clone(),
            host_key,
        },
    )
}

#[tokio::test]
async fn test_real_host_and_client_full_lifecycle() {
    // a. Bind real host on loopback ephemeral port and spawn run_host with oneshot shutdown.
    let config_dir = std::env::temp_dir().join(format!("perigee-loopback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config_dir);
    let client = Arc::new(NodeIdentity::generate().expect("client identity"));
    TrustedKeys::load(config_dir.join(TRUSTED_CLIENTS_FILE_NAME))
        .expect("load trust file")
        .add(client.public_key(), "loopback-client")
        .expect("trust the loopback client");

    let host_opts = HostOptions {
        bind_addr: "127.0.0.1:0".parse().expect("valid loopback addr"),
        config_dir: Some(config_dir.clone()),
        ..Default::default()
    };
    let (daemon, listener) = bind_host(&host_opts).expect("bind_host should succeed");
    let addr = listener
        .local_addr()
        .expect("listener.local_addr should succeed");
    let host_key = NodeIdentity::load_or_create(&config_dir.join(IDENTITY_FILE_NAME))
        .expect("host identity")
        .public_key();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let daemon_for_host = daemon.clone();
    let host_task = tokio::spawn(async move {
        run_host(daemon_for_host, listener, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    // b. Connect first client session and confirm Active state and daemon session count == 1.
    let mut session = tokio::time::timeout(
        Duration::from_secs(5),
        connect(trusted_options(&addr, &client, host_key)),
    )
    .await
    .expect("connect timed out")
    .expect("connect should succeed");

    assert_eq!(session.state(), SessionState::Active);
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.active_session_count().await != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for active_session_count == 1");
    assert_eq!(daemon.active_session_count().await, 1);

    // c. take_video_rx() yields at least one DecodedFrameSurface.
    let mut video_rx = session
        .take_video_rx()
        .expect("video_rx should be available");
    let frame = tokio::time::timeout(Duration::from_secs(5), video_rx.recv())
        .await
        .expect("timed out waiting for DecodedFrameSurface")
        .expect("video_rx closed before yielding a frame");
    assert!(!frame.data.is_empty());

    // d. Send a MouseMove input event over the session input channel.
    tokio::time::timeout(
        Duration::from_secs(5),
        session.input_tx.send(InputEvent::MouseMove {
            monitor_id: 1,
            x: 10.0,
            y: 20.0,
            is_absolute: true,
        }),
    )
    .await
    .expect("input_tx.send timed out")
    .expect("input_tx.send should succeed");

    // e. Disconnect cleanly, verify tasks finished, and poll until active_session_count == 0.
    tokio::time::timeout(Duration::from_secs(5), session.disconnect("test done"))
        .await
        .expect("session.disconnect timed out");
    assert!(session.bridge.tasks_finished());

    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.active_session_count().await != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for active_session_count == 0 after disconnect");
    assert_eq!(daemon.active_session_count().await, 0);

    // f. Reconnect a second session to the same host address -> Active, count back to 1.
    let mut session2 = tokio::time::timeout(
        Duration::from_secs(5),
        connect(trusted_options(&addr, &client, host_key)),
    )
    .await
    .expect("second connect timed out")
    .expect("second connect should succeed");

    assert_eq!(session2.state(), SessionState::Active);
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.active_session_count().await != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for active_session_count == 1 after reconnect");
    assert_eq!(daemon.active_session_count().await, 1);

    // g. Trigger host shutdown via oneshot; second session transitions to Terminated,
    //    run_host returns Ok(()), and active_session_count is 0.
    let _ = shutdown_tx.send(());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(session2.state(), SessionState::Terminated { .. }) {
                break;
            }
            if session2.state_rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for second session to become Terminated on host shutdown");
    assert!(matches!(session2.state(), SessionState::Terminated { .. }));

    let host_result = tokio::time::timeout(Duration::from_secs(5), host_task)
        .await
        .expect("run_host timed out on shutdown")
        .expect("run_host task panicked");
    assert!(host_result.is_ok());
    assert_eq!(daemon.active_session_count().await, 0);

    tokio::time::timeout(Duration::from_secs(5), session2.disconnect("cleanup"))
        .await
        .expect("session2 cleanup timed out");
    assert_eq!(session2.peer_node_key(), Some(host_key));
    let _ = std::fs::remove_dir_all(&config_dir);
}

#[tokio::test]
async fn test_pairing_then_connect() {
    let root = std::env::temp_dir().join(format!("perigee-loopback-pair-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let host_dir = root.join("host");
    let client_dir = root.join("client");

    let host_identity =
        NodeIdentity::load_or_create(&host_dir.join(IDENTITY_FILE_NAME)).expect("host identity");
    let window = PairingWindow::bind(
        &host_identity,
        host_dir.join(TRUSTED_CLIENTS_FILE_NAME),
        "127.0.0.1:0".parse().expect("valid loopback addr"),
        Duration::from_secs(30),
    )
    .await
    .expect("bind pairing window");
    let pair_port = window.local_addr().expect("pairing addr").port();
    let secret = window.secret().to_string();

    let host_opts = HostOptions {
        bind_addr: "127.0.0.1:0".parse().expect("valid loopback addr"),
        config_dir: Some(host_dir.clone()),
        ..Default::default()
    };
    let (daemon, listener) = bind_host(&host_opts).expect("bind_host should succeed");
    let addr = listener.local_addr().expect("listener addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let host_task = tokio::spawn(async move {
        run_host(daemon, listener, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let pair_args = ClientPairArgs {
        target: addr.to_string(),
        host_key: host_identity.public_key(),
        secret,
        label: "testuser-laptop".to_string(),
        pair_port,
        config_dir: Some(client_dir.clone()),
    };
    let (paired, printed) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(window.run(), run_client_pair(&pair_args))
    })
    .await
    .expect("pairing timed out");
    let printed = printed.expect("client pairing should succeed");
    assert!(printed.contains("pinned"), "got: {printed}");
    let client_key = NodeIdentity::load_or_create(&client_dir.join(IDENTITY_FILE_NAME))
        .expect("client identity")
        .public_key();
    assert_eq!(
        paired
            .expect("host pairing window")
            .map(|client| client.key),
        Some(client_key)
    );

    let connect_args = ConnectArgs {
        target: addr.to_string(),
        interface: None,
        run_for: None,
        handshake_timeout: perigee_client::DEFAULT_HANDSHAKE_TIMEOUT,
        host_key: None,
        config_dir: Some(client_dir.clone()),
        dump_frame: None,
        window: false,
    };
    let auth = client_auth_for(&connect_args).expect("auth from the pins pairing wrote");
    let session = tokio::time::timeout(
        Duration::from_secs(5),
        connect(ConnectOptions::new(addr.to_string(), auth)),
    )
    .await
    .expect("connect timed out")
    .expect("connect should succeed");
    assert_eq!(session.state(), SessionState::Active);
    session.disconnect("pairing test done").await;

    let _ = shutdown_tx.send(());
    host_task
        .await
        .expect("host task panicked")
        .expect("host exits cleanly");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn test_real_host_and_client_av1_roundtrip() {
    let config_dir =
        std::env::temp_dir().join(format!("perigee-loopback-av1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config_dir);
    let client = Arc::new(NodeIdentity::generate().expect("client identity"));
    TrustedKeys::load(config_dir.join(TRUSTED_CLIENTS_FILE_NAME))
        .expect("load trust file")
        .add(client.public_key(), "loopback-av1-client")
        .expect("trust the loopback client");

    let host_opts = HostOptions {
        bind_addr: "127.0.0.1:0".parse().expect("valid loopback addr"),
        config_dir: Some(config_dir.clone()),
        media: MediaMode::StandIn,
        codec: HostCodec::Av1,
        ..Default::default()
    };
    let (daemon, listener) = bind_host(&host_opts).expect("bind_host should succeed");
    let addr = listener
        .local_addr()
        .expect("listener.local_addr should succeed");
    let host_key = NodeIdentity::load_or_create(&config_dir.join(IDENTITY_FILE_NAME))
        .expect("host identity")
        .public_key();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let daemon_for_host = daemon.clone();
    let host_task = tokio::spawn(async move {
        run_host(daemon_for_host, listener, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let mut session = tokio::time::timeout(
        Duration::from_secs(5),
        connect(trusted_options(&addr, &client, host_key)),
    )
    .await
    .expect("connect timed out")
    .expect("connect should succeed");

    assert_eq!(session.state(), SessionState::Active);

    let mut video_rx = session
        .take_video_rx()
        .expect("video_rx should be available");

    for frame_num in 1..=2 {
        let frame = tokio::time::timeout(Duration::from_secs(10), video_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for decoded AV1 frame {frame_num}"))
            .unwrap_or_else(|| {
                panic!("video_rx closed before yielding decoded AV1 frame {frame_num}")
            });
        assert_eq!(frame.format, PixelFormat::Rgba8);
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);
    }

    tokio::time::timeout(Duration::from_secs(5), session.disconnect("av1 test done"))
        .await
        .expect("session.disconnect timed out");
    assert!(session.bridge.tasks_finished());

    let _ = shutdown_tx.send(());
    let host_result = tokio::time::timeout(Duration::from_secs(5), host_task)
        .await
        .expect("run_host timed out on shutdown")
        .expect("run_host task panicked");
    assert!(host_result.is_ok());
    assert_eq!(daemon.active_session_count().await, 0);

    let _ = std::fs::remove_dir_all(&config_dir);
}
