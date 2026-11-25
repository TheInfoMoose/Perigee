//! [`FrameCapturer`] that follows logind's active session on seat0 by running
//! `capture-worker` as that session's user and reading raw frames from its stdout.
//!
//! The parent keeps `CAP_SETUID`/`CAP_SETGID` only to start the worker; the worker gets the
//! session's uid and gid, no supplementary groups and no ambient capabilities.

use std::io::{BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use perigee_protocol::{
    MAX_WIRE_FRAME_BYTES, MonitorInfo, PixelFormat, TopologyMessage, WireMessage,
    decode_wire_message,
};

use crate::backends::linux_capture::FrameCapturer;
use crate::backends::wayland_dmabuf::CaptureMode;
use crate::seat::{ActiveSession, LoginctlRunner, SystemLoginctl, query_active_session};
use crate::{CaptureBackendType, CapturedFrame};

/// How often the capturer re-reads logind's active session.
const SEAT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long to wait for a capture worker to exit after closing its stdin lifeline.
const WORKER_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Polling interval while waiting for a capture worker to exit.
const WORKER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The program and leading arguments that start a capture worker:
/// `perigee-host capture-worker` or `perigee host capture-worker`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

impl WorkerCommand {
    /// The command for the running binary: `perigee host capture-worker` when it is `perigee`,
    /// otherwise `<exe> capture-worker`.
    pub fn current() -> Result<Self> {
        let program = std::env::current_exe().context("failed to find the running executable")?;
        let is_cli = program.file_name().is_some_and(|name| name == "perigee");
        let prefix_args = if is_cli {
            vec!["host".to_string()]
        } else {
            Vec::new()
        };
        Ok(Self {
            program,
            prefix_args,
        })
    }

    /// Builds the worker process for `session` using [`CaptureMode::Auto`].
    pub fn build(&self, session: &ActiveSession, output: usize) -> Command {
        self.build_with_mode(session, output, CaptureMode::Auto)
    }

    /// Builds the worker process for `session` with the given `capture_mode`: its uid and gid
    /// (when they differ from ours), no supplementary groups, no ambient capabilities, a cleared
    /// environment with `XDG_RUNTIME_DIR` set, stdin piped as the parent's lifeline (nothing is
    /// ever written to it; closing it tells the worker to exit), and stdout piped for frames.
    pub fn build_with_mode(
        &self,
        session: &ActiveSession,
        output: usize,
        capture_mode: CaptureMode,
    ) -> Command {
        let mode_arg = match capture_mode {
            CaptureMode::Auto => "auto",
            CaptureMode::Dmabuf => "dmabuf",
            CaptureMode::Shm => "shm",
        };
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.prefix_args)
            .arg("capture-worker")
            .arg("--output")
            .arg(output.to_string())
            .arg("--capture-mode")
            .arg(mode_arg)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &session.runtime_dir)
            .env("HOME", "/")
            .env(
                "RUST_LOG",
                std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: geteuid only reads the calling process's credentials; it cannot fail.
        let current_uid = unsafe { libc::geteuid() };
        let switch_user = session.uid != current_uid;
        if switch_user {
            cmd.uid(session.uid).gid(session.gid);
        }
        // SAFETY: the closure runs in the forked child just before exec. getgroups(0, NULL),
        // setgroups(0, NULL) and prctl pass no pointers into parent memory, allocate nothing
        // and are async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                let zero: libc::c_ulong = 0;
                if switch_user
                    && libc::getgroups(0, std::ptr::null_mut()) > 0
                    && libc::setgroups(0, std::ptr::null()) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                    zero,
                    zero,
                    zero,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd
    }
}

/// Callback invoked whenever the active capture worker reports a non-empty display layout.
pub type LayoutSink = Arc<dyn Fn(Vec<MonitorInfo>, u32) + Send + Sync>;

/// A decoded message received from a capture worker's stdout pipe.
#[derive(Debug, Clone)]
pub enum WorkerMessage {
    Frame(CapturedFrame),
    DisplayLayout(Vec<MonitorInfo>),
}

/// Reads one length-prefixed `WireMessage::Video` or `TopologyMessage::DisplayLayout` message
/// from a capture worker.
pub fn read_worker_message(reader: &mut impl Read) -> Result<WorkerMessage> {
    let mut len = [0u8; 4];
    reader
        .read_exact(&mut len)
        .context("capture worker closed its output")?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_WIRE_FRAME_BYTES {
        bail!("capture worker sent a {len}-byte frame");
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .context("capture worker sent a truncated frame")?;
    match decode_wire_message(&body)? {
        WireMessage::Video(msg) => {
            if !matches!(msg.format, PixelFormat::Bgra8 | PixelFormat::Rgba8) {
                bail!("capture worker sent a {:?} frame", msg.format);
            }
            Ok(WorkerMessage::Frame(CapturedFrame {
                frame_index: msg.frame_id,
                timestamp_us: msg.timestamp_us,
                display_id: msg.monitor_id,
                width: msg.width,
                height: msg.height,
                stride: msg.stride,
                pixel_format: msg.format,
                data: msg.payload,
                is_dmabuf_or_metal_handle: false,
            }))
        }
        WireMessage::Topology(TopologyMessage::DisplayLayout { monitors }) => {
            Ok(WorkerMessage::DisplayLayout(monitors))
        }
        _ => bail!("capture worker sent an unsupported message"),
    }
}

/// Reads one length-prefixed `WireMessage::Video` raw frame from a capture worker.
pub fn read_worker_frame(reader: &mut impl Read) -> Result<CapturedFrame> {
    match read_worker_message(reader)? {
        WorkerMessage::Frame(frame) => Ok(frame),
        _ => bail!("capture worker sent a non-video message"),
    }
}

#[derive(Debug, Default)]
struct StopState {
    stopped: bool,
    lifeline: Option<ChildStdin>,
}

/// Stops the capture worker a [`WorkerCapturer`] is running, from any thread. A session stop
/// calls it so the capturer stays stopped and closes the worker's stdin lifeline, causing the
/// worker to exit and its stdout pipe to reach EOF. If the worker does not exit within
/// [`WORKER_EXIT_TIMEOUT`], `wait_for_worker_exit` falls back to `kill()` + `wait()`.
#[derive(Debug, Clone)]
pub struct WorkerStopper(Arc<Mutex<StopState>>);

impl WorkerStopper {
    /// Marks the capturer stopped (sticky) and closes the running worker's stdin lifeline, if any.
    /// Safe to call from any thread, any number of times, before, during, or after a worker spawn.
    pub fn stop(&self) {
        let lifeline = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            state.lifeline.take()
        };
        drop(lifeline);
    }
}

/// Returns `true` when blocking `child.wait()` is safe to call after `child.kill()`:
/// either `kill()` succeeded or returned `InvalidInput` (meaning the child had already exited).
/// Any other error (such as `PermissionDenied` / `EPERM`) means the child may still be running,
/// so a blocking `wait()` could hang indefinitely.
fn should_wait_after_kill(kill_result: &std::io::Result<()>) -> bool {
    match kill_result {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::InvalidInput,
    }
}

/// Sends `SIGKILL` to `child` and reaps it with `wait()` only when `kill()` succeeds or reports
/// `InvalidInput` (child already exited). On any other error (e.g., `PermissionDenied`), logs a
/// warning and returns immediately without blocking.
fn kill_and_reap_child(session: &ActiveSession, child: &mut Child) {
    let kill_result = child.kill();
    if should_wait_after_kill(&kill_result) {
        let _ = child.wait();
    } else if let Err(e) = kill_result {
        warn!(
            session = %session.id,
            pid = child.id(),
            error = %e,
            "Failed to kill capture worker; skipping blocking wait"
        );
    }
}

/// Waits for `child` to exit for at most [`WORKER_EXIT_TIMEOUT`] after its stdin closes; if it
/// has not exited by then (or polling its status fails), falls back to `kill()` + `wait()` to
/// terminate and reap it. Blocking `wait()` is skipped if `kill()` fails with an error other
/// than `InvalidInput` (for example `PermissionDenied`), avoiding an indefinite hang.
fn wait_for_worker_exit(session: &ActiveSession, mut child: Child) {
    let deadline = Instant::now() + WORKER_EXIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                info!(session = %session.id, "Stopped capture worker");
                return;
            }
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    warn!(
                        session = %session.id,
                        pid = child.id(),
                        "Capture worker did not exit after its stdin closed"
                    );
                    kill_and_reap_child(session, &mut child);
                    return;
                }
                let remaining = deadline.saturating_duration_since(now);
                std::thread::sleep(remaining.min(WORKER_EXIT_POLL_INTERVAL));
            }
            Err(e) => {
                warn!(
                    session = %session.id,
                    pid = child.id(),
                    error = %e,
                    "Capture worker did not exit after its stdin closed"
                );
                kill_and_reap_child(session, &mut child);
                return;
            }
        }
    }
}

/// Returns `true` on exponential backoff milestones (1, 2, 4, 8, ...) so repeated seat-poll
/// failures warn without flooding the log every second.
fn should_warn_poll_failure(consecutive_failures: u32) -> bool {
    consecutive_failures > 0 && consecutive_failures.is_power_of_two()
}

/// Captures whichever Wayland session logind reports active on seat0, through a
/// `capture-worker` child running as that session's user. Restarts the worker when the active
/// session changes or the worker exits (at most once per poll); while no Wayland session is
/// active it repeats the last frame (black 64x64 before the first) so the remote session stays
/// open across greeter ↔ user switches.
pub struct WorkerCapturer {
    command: WorkerCommand,
    output: usize,
    capture_mode: CaptureMode,
    seat: String,
    loginctl: Box<dyn LoginctlRunner + Send + Sync>,
    current: Option<(ActiveSession, Child, BufReader<ChildStdout>)>,
    stop_state: Arc<Mutex<StopState>>,
    last_poll: Option<Instant>,
    consecutive_poll_failures: u32,
    last_frame: CapturedFrame,
    monitors: Vec<MonitorInfo>,
    layout_sink: Option<LayoutSink>,
    frame_counter: u64,
    start: Instant,
}

impl WorkerCapturer {
    /// Follows seat0 through the real `loginctl`, starting workers with `command`.
    pub fn new(command: WorkerCommand, output: usize) -> Self {
        Self::with_loginctl(command, output, Box::new(SystemLoginctl))
    }

    /// Like [`WorkerCapturer::new`] with a custom `loginctl` (tests).
    pub fn with_loginctl(
        command: WorkerCommand,
        output: usize,
        loginctl: Box<dyn LoginctlRunner + Send + Sync>,
    ) -> Self {
        Self {
            command,
            output,
            capture_mode: CaptureMode::Auto,
            seat: "seat0".to_string(),
            loginctl,
            current: None,
            stop_state: Arc::new(Mutex::new(StopState::default())),
            last_poll: None,
            consecutive_poll_failures: 0,
            last_frame: CapturedFrame {
                frame_index: 0,
                timestamp_us: 0,
                display_id: 1,
                width: 64,
                height: 64,
                stride: 64 * 4,
                pixel_format: PixelFormat::Bgra8,
                data: vec![0; 64 * 64 * 4],
                is_dmabuf_or_metal_handle: false,
            },
            monitors: Vec::new(),
            layout_sink: None,
            frame_counter: 0,
            start: Instant::now(),
        }
    }

    /// Configures a callback invoked whenever the active worker reports a non-empty display layout.
    pub fn with_layout_sink(mut self, sink: LayoutSink) -> Self {
        self.layout_sink = Some(sink);
        self
    }

    /// Configures the Wayland capture mode passed to `capture-worker --capture-mode`.
    pub fn with_capture_mode(mut self, capture_mode: CaptureMode) -> Self {
        self.capture_mode = capture_mode;
        self
    }

    /// Returns the configured Wayland capture mode passed to `capture-worker --capture-mode`.
    pub fn capture_mode(&self) -> CaptureMode {
        self.capture_mode
    }

    /// A handle that stops the current (and any later) worker from another thread.
    pub fn stopper(&self) -> WorkerStopper {
        WorkerStopper(self.stop_state.clone())
    }

    /// Returns the configured Wayland output index passed to `capture-worker --output`.
    pub fn output_index(&self) -> usize {
        self.output
    }

    fn is_stopped(&self) -> bool {
        self.stop_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
    }

    fn poll_seat(&mut self) -> Result<()> {
        if self.is_stopped() {
            self.stop_worker();
            bail!("capture stopped");
        }
        self.last_poll = Some(Instant::now());
        let active = query_active_session(self.loginctl.as_ref(), &self.seat)?;
        if self.is_stopped() {
            self.stop_worker();
            bail!("capture stopped");
        }
        let changed = match (&self.current, &active) {
            (Some((running, _, _)), Some(now)) => running.id != now.id,
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return Ok(());
        }
        self.stop_worker();
        if let Some(session) = active {
            if self.is_stopped() {
                bail!("capture stopped");
            }
            info!(
                session = %session.id,
                user = %session.user_name,
                class = %session.class,
                "Starting capture worker for the active session"
            );
            let mut child = self
                .command
                .build_with_mode(&session, self.output, self.capture_mode)
                .spawn()
                .context("failed to start the capture worker")?;
            let stdin = child.stdin.take().context("capture worker has no stdin")?;
            let stdout = child
                .stdout
                .take()
                .context("capture worker has no stdout")?;
            let rejected_stdin = {
                let mut state = self.stop_state.lock().unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    Some(stdin)
                } else {
                    state.lifeline = Some(stdin);
                    None
                }
            };
            if let Some(stdin) = rejected_stdin {
                drop(stdin);
                drop(stdout);
                wait_for_worker_exit(&session, child);
                bail!("capture stopped");
            }
            self.current = Some((session, child, BufReader::new(stdout)));
        }
        Ok(())
    }

    fn stop_worker(&mut self) {
        self.monitors.clear();
        let lifeline = self
            .stop_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lifeline
            .take();
        drop(lifeline);
        if let Some((session, child, reader)) = self.current.take() {
            drop(reader);
            wait_for_worker_exit(&session, child);
        }
    }

    fn repeat_last_frame(&mut self) -> CapturedFrame {
        self.frame_counter += 1;
        let mut frame = self.last_frame.clone();
        frame.frame_index = self.frame_counter;
        frame.timestamp_us = self.start.elapsed().as_micros() as u64;
        frame
    }
}

impl Drop for WorkerCapturer {
    fn drop(&mut self) {
        self.stopper().stop();
        self.stop_worker();
    }
}

impl FrameCapturer for WorkerCapturer {
    fn backend_type(&self) -> CaptureBackendType {
        CaptureBackendType::WaylandPipeWire
    }

    fn is_live_display(&self) -> bool {
        self.current.is_some()
    }

    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
        if !self.monitors.is_empty() {
            return Ok(self.monitors.clone());
        }
        Ok(vec![MonitorInfo {
            monitor_id: 1,
            name: "seat0 active session".to_string(),
            x: 0,
            y: 0,
            width: self.last_frame.width,
            height: self.last_frame.height,
            scale_factor: 1.0,
            refresh_rate_hz: 10,
            is_primary: true,
        }])
    }

    fn capture_next_frame(&mut self) -> Result<CapturedFrame> {
        if self.is_stopped() {
            self.stop_worker();
            bail!("capture stopped");
        }
        if self
            .last_poll
            .is_none_or(|at| at.elapsed() >= SEAT_POLL_INTERVAL)
        {
            match self.poll_seat() {
                Ok(()) => {
                    self.consecutive_poll_failures = 0;
                }
                Err(e) => {
                    if self.is_stopped() {
                        self.stop_worker();
                        bail!("capture stopped");
                    }
                    self.consecutive_poll_failures =
                        self.consecutive_poll_failures.saturating_add(1);
                    if should_warn_poll_failure(self.consecutive_poll_failures) {
                        warn!(
                            failures = self.consecutive_poll_failures,
                            error = %format!("{e:#}"),
                            "Seat poll failed; retrying at the next poll"
                        );
                    } else {
                        debug!(
                            failures = self.consecutive_poll_failures,
                            error = %format!("{e:#}"),
                            "Seat poll failed; retrying at the next poll"
                        );
                    }
                }
            }
        }
        let read = loop {
            let next = match self.current.as_mut() {
                None => break None,
                Some((session, _, reader)) => (session.id.clone(), read_worker_message(reader)),
            };
            match next {
                (_, Ok(WorkerMessage::DisplayLayout(monitors))) => {
                    if !monitors.is_empty() {
                        self.monitors = monitors.clone();
                        if let Some(sink) = &self.layout_sink {
                            sink(monitors, self.output as u32 + 1);
                        }
                    }
                    if self.is_stopped() {
                        self.stop_worker();
                        bail!("capture stopped");
                    }
                }
                (session_id, Ok(WorkerMessage::Frame(frame))) => {
                    break Some((session_id, Ok(frame)));
                }
                (session_id, Err(e)) => break Some((session_id, Err(e))),
            }
        };
        match read {
            None => {
                std::thread::sleep(Duration::from_millis(100));
                if self.is_stopped() {
                    bail!("capture stopped");
                }
                Ok(self.repeat_last_frame())
            }
            Some((_, Ok(mut frame))) => {
                if self.is_stopped() {
                    self.stop_worker();
                    bail!("capture stopped");
                }
                self.frame_counter += 1;
                frame.frame_index = self.frame_counter;
                self.last_frame = frame.clone();
                Ok(frame)
            }
            Some((session_id, Err(e))) => {
                if self.is_stopped() {
                    self.stop_worker();
                    bail!("capture stopped");
                }
                warn!(session = %session_id, error = %e, "Capture worker stopped; restarting at the next seat poll");
                self.stop_worker();
                Ok(self.repeat_last_frame())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perigee_protocol::{VideoCodec, VideoFrameMessage, encode_length_prefixed};

    #[test]
    fn test_read_worker_frame_roundtrip_and_errors() {
        let msg = WireMessage::Video(VideoFrameMessage {
            frame_id: 3,
            monitor_id: 1,
            timestamp_us: 42,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: 2,
            height: 1,
            stride: 8,
            format: PixelFormat::Bgra8,
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
        });
        let framed = encode_length_prefixed(&msg).unwrap();
        let frame = read_worker_frame(&mut framed.as_slice()).unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.stride, frame.timestamp_us),
            (2, 1, 8, 42)
        );
        assert_eq!(frame.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let mut short: &[u8] = &framed[..framed.len() - 1];
        assert!(read_worker_frame(&mut short).is_err());
        let mut huge: &[u8] = &[0xff, 0xff, 0xff, 0x7f];
        assert!(read_worker_frame(&mut huge).is_err());
    }

    #[test]
    fn test_worker_command_sets_session_env() {
        let command = WorkerCommand {
            program: PathBuf::from("/usr/bin/perigee"),
            prefix_args: vec!["host".to_string()],
        };
        let session = ActiveSession {
            id: "c1".to_string(),
            uid: 977,
            gid: 977,
            user_name: "cosmic-greeter".to_string(),
            class: "greeter".to_string(),
            session_type: "wayland".to_string(),
            runtime_dir: PathBuf::from("/run/user/977"),
        };
        let cmd = command.build(&session, 0);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "host",
                "capture-worker",
                "--output",
                "0",
                "--capture-mode",
                "auto"
            ]
        );
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&(
            "XDG_RUNTIME_DIR".to_string(),
            Some("/run/user/977".to_string())
        )));
    }

    #[test]
    fn test_worker_capturer_repeats_frame_without_session() {
        struct NoSession;
        impl LoginctlRunner for NoSession {
            fn run(&self, _args: &[&str]) -> Result<String> {
                Ok("ActiveSession=\n".to_string())
            }
        }
        let command = WorkerCommand {
            program: PathBuf::from("/nonexistent/perigee-host"),
            prefix_args: Vec::new(),
        };
        let mut capturer = WorkerCapturer::with_loginctl(command, 0, Box::new(NoSession));
        let first = capturer.capture_next_frame().unwrap();
        let second = capturer.capture_next_frame().unwrap();
        assert_eq!((first.width, first.height), (64, 64));
        assert_eq!(second.frame_index, first.frame_index + 1);
        assert!(!capturer.is_live_display());
    }

    struct FakeLoginctl {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        uid: u32,
        gid: u32,
        runtime_dir: PathBuf,
    }

    impl FakeLoginctl {
        fn new() -> (Self, Arc<std::sync::atomic::AtomicUsize>) {
            // SAFETY: geteuid and getegid only read the calling process's credentials; they cannot fail.
            let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (
                Self {
                    calls: calls.clone(),
                    uid,
                    gid,
                    runtime_dir: std::env::temp_dir(),
                },
                calls,
            )
        }
    }

    impl LoginctlRunner for FakeLoginctl {
        fn run(&self, args: &[&str]) -> Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match args.first().copied() {
                Some("show-seat") => Ok("ActiveSession=c1\n".to_string()),
                Some("show-session") => Ok(format!(
                    "User={}\nName=testuser\nClass=user\nType=wayland\nState=active\n",
                    self.uid
                )),
                Some("show-user") => Ok(format!(
                    "GID={}\nRuntimePath={}\n",
                    self.gid,
                    self.runtime_dir.display()
                )),
                other => bail!("unexpected loginctl call: {other:?}"),
            }
        }
    }

    fn fake_worker_command(script: &str) -> WorkerCommand {
        WorkerCommand {
            program: PathBuf::from("/bin/sh"),
            prefix_args: vec![
                "-c".to_string(),
                script.to_string(),
                "fake-worker".to_string(),
            ],
        }
    }

    #[test]
    fn test_worker_stop_unblocks_a_blocked_capture() {
        let (loginctl, calls) = FakeLoginctl::new();
        let mut capturer = WorkerCapturer::with_loginctl(
            fake_worker_command("while IFS= read -r _; do :; done"),
            0,
            Box::new(loginctl),
        );
        let stopper = capturer.stopper();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_thread = std::thread::spawn(move || {
            let result = capturer.capture_next_frame();
            let _ = tx.send((capturer, result));
        });

        std::thread::sleep(Duration::from_millis(200));
        stopper.stop();

        let (mut capturer, first) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("capture_next_frame should unblock within 2 s of stop()");
        worker_thread.join().unwrap();

        let first_err = first.unwrap_err();
        assert!(
            format!("{first_err:#}").contains("capture stopped"),
            "got: {first_err:#}"
        );

        let calls_after_first = calls.load(std::sync::atomic::Ordering::SeqCst);
        let started = Instant::now();
        let second_err = capturer.capture_next_frame().unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "second capture_next_frame took {:?}",
            started.elapsed()
        );
        assert!(
            format!("{second_err:#}").contains("capture stopped"),
            "got: {second_err:#}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            calls_after_first
        );
    }

    #[test]
    fn test_worker_stop_before_first_capture_spawns_nothing() {
        let (loginctl, calls) = FakeLoginctl::new();
        let mut capturer = WorkerCapturer::with_loginctl(
            fake_worker_command("while IFS= read -r _; do :; done"),
            0,
            Box::new(loginctl),
        );
        capturer.stopper().stop();

        let err = capturer.capture_next_frame().unwrap_err();
        assert!(
            format!("{err:#}").contains("capture stopped"),
            "got: {err:#}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn test_worker_error_reaps_worker_blocked_on_write() {
        let (loginctl, _calls) = FakeLoginctl::new();
        let mut capturer = WorkerCapturer::with_loginctl(
            fake_worker_command(
                "{ while echo 0000000000000000; do :; done; } & while IFS= read -r _; do :; done; wait",
            ),
            0,
            Box::new(loginctl),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_thread = std::thread::spawn(move || {
            let result = capturer.capture_next_frame();
            let _ = tx.send((capturer, result));
        });

        let (capturer, result) = rx
            .recv_timeout(Duration::from_millis(1000))
            .expect("capture_next_frame should reap blocked writer within 1000 ms");
        worker_thread.join().unwrap();

        let frame = result.expect("worker read error should return repeated frame");
        assert_eq!((frame.width, frame.height), (64, 64));
        assert!(!capturer.is_live_display());
    }

    #[test]
    fn test_wait_for_worker_exit_kills_and_reaps_hung_worker() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn hung test worker");
        let pid = child.id();
        drop(child.stdin.take());
        let session = ActiveSession {
            id: "c1".to_string(),
            uid: 1000,
            gid: 1000,
            user_name: "testuser".to_string(),
            class: "user".to_string(),
            session_type: "wayland".to_string(),
            runtime_dir: std::env::temp_dir(),
        };

        wait_for_worker_exit(&session, child);

        // SAFETY: signal 0 only checks whether `pid` exists in the process table; no signal is
        // delivered and no pointers are passed.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        assert_eq!(rc, -1, "hung worker pid {pid} should be gone");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "hung worker pid {pid} should be reaped (ESRCH), not a zombie"
        );
    }

    #[test]
    fn test_worker_capturer_survives_failed_seat_poll() {
        struct FailingLoginctl;
        impl LoginctlRunner for FailingLoginctl {
            fn run(&self, _args: &[&str]) -> Result<String> {
                bail!("loginctl failed")
            }
        }
        let command = WorkerCommand {
            program: PathBuf::from("/nonexistent/perigee-host"),
            prefix_args: Vec::new(),
        };
        let mut capturer = WorkerCapturer::with_loginctl(command, 0, Box::new(FailingLoginctl));
        let first = capturer.capture_next_frame().unwrap();
        let second = capturer.capture_next_frame().unwrap();
        assert_eq!((first.width, first.height), (64, 64));
        assert_eq!((second.width, second.height), (64, 64));
        assert_eq!(second.frame_index, first.frame_index + 1);
        assert!(!capturer.is_live_display());
    }

    #[test]
    fn test_worker_capturer_survives_failed_spawn() {
        let (loginctl, _calls) = FakeLoginctl::new();
        let command = WorkerCommand {
            program: PathBuf::from("/nonexistent/perigee-host"),
            prefix_args: Vec::new(),
        };
        let mut capturer = WorkerCapturer::with_loginctl(command, 0, Box::new(loginctl));
        let frame = capturer.capture_next_frame().unwrap();
        assert_eq!((frame.width, frame.height), (64, 64));
        assert!(!capturer.is_live_display());
    }

    #[test]
    fn test_poll_failure_backoff_resets_on_success() {
        assert!(!should_warn_poll_failure(0));
        for n in [1, 2, 4, 8, 16] {
            assert!(
                should_warn_poll_failure(n),
                "expected should_warn_poll_failure({n}) to be true"
            );
        }
        for n in [3, 5, 6, 7] {
            assert!(
                !should_warn_poll_failure(n),
                "expected should_warn_poll_failure({n}) to be false"
            );
        }

        struct FlakyLoginctl {
            attempts: std::sync::atomic::AtomicU32,
        }
        impl LoginctlRunner for FlakyLoginctl {
            fn run(&self, _args: &[&str]) -> Result<String> {
                let attempt = self
                    .attempts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if attempt < 2 {
                    bail!("loginctl failed on attempt {attempt}")
                }
                Ok("ActiveSession=\n".to_string())
            }
        }

        let command = WorkerCommand {
            program: PathBuf::from("/nonexistent/perigee-host"),
            prefix_args: Vec::new(),
        };
        let mut capturer = WorkerCapturer::with_loginctl(
            command,
            0,
            Box::new(FlakyLoginctl {
                attempts: std::sync::atomic::AtomicU32::new(0),
            }),
        );

        assert_eq!(capturer.consecutive_poll_failures, 0);

        let _ = capturer.capture_next_frame().unwrap();
        assert_eq!(capturer.consecutive_poll_failures, 1);

        capturer.last_poll = None;
        let _ = capturer.capture_next_frame().unwrap();
        assert_eq!(capturer.consecutive_poll_failures, 2);

        capturer.last_poll = None;
        let _ = capturer.capture_next_frame().unwrap();
        assert_eq!(capturer.consecutive_poll_failures, 0);
    }

    #[test]
    fn test_should_wait_after_kill_skips_wait_on_permission_denied() {
        assert!(should_wait_after_kill(&Ok(())));
        assert!(should_wait_after_kill(&Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "already exited",
        ))));
        assert!(!should_wait_after_kill(&Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "EPERM",
        ))));
        assert!(!should_wait_after_kill(&Err(std::io::Error::other(
            "unexpected error",
        ))));
    }

    #[test]
    fn test_worker_capturer_preserves_nonzero_output_index() {
        let command = WorkerCommand {
            program: PathBuf::from("/usr/bin/perigee-host"),
            prefix_args: Vec::new(),
        };
        let session = ActiveSession {
            id: "c1".to_string(),
            uid: 1000,
            gid: 1000,
            user_name: "testuser".to_string(),
            class: "user".to_string(),
            session_type: "wayland".to_string(),
            runtime_dir: PathBuf::from("/run/user/1000"),
        };
        let cmd = command.build(&session, 2);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["capture-worker", "--output", "2", "--capture-mode", "auto"]
        );

        let worker = WorkerCapturer::new(command, 2);
        assert_eq!(worker.output_index(), 2);
    }

    #[test]
    fn test_worker_command_build_with_mode_passes_capture_mode_flag() {
        let command = WorkerCommand {
            program: PathBuf::from("/usr/bin/perigee-host"),
            prefix_args: Vec::new(),
        };
        let session = ActiveSession {
            id: "c1".to_string(),
            uid: 1000,
            gid: 1000,
            user_name: "testuser".to_string(),
            class: "user".to_string(),
            session_type: "wayland".to_string(),
            runtime_dir: PathBuf::from("/run/user/1000"),
        };
        let cmd = command.build_with_mode(&session, 1, CaptureMode::Dmabuf);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "capture-worker",
                "--output",
                "1",
                "--capture-mode",
                "dmabuf"
            ]
        );

        let worker = WorkerCapturer::new(command, 1).with_capture_mode(CaptureMode::Shm);
        assert_eq!(worker.capture_mode(), CaptureMode::Shm);
    }

    #[test]
    fn test_read_worker_message_decodes_layout_and_frame() {
        let monitors = vec![
            MonitorInfo {
                monitor_id: 1,
                name: "DP-1".to_string(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale_factor: 1.0,
                refresh_rate_hz: 60,
                is_primary: true,
            },
            MonitorInfo {
                monitor_id: 2,
                name: "HDMI-1".to_string(),
                x: 1920,
                y: 0,
                width: 2560,
                height: 1440,
                scale_factor: 1.5,
                refresh_rate_hz: 120,
                is_primary: false,
            },
        ];
        let layout_msg = WireMessage::Topology(TopologyMessage::DisplayLayout {
            monitors: monitors.clone(),
        });
        let video_msg = WireMessage::Video(VideoFrameMessage {
            frame_id: 7,
            monitor_id: 2,
            timestamp_us: 99,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: 2,
            height: 1,
            stride: 8,
            format: PixelFormat::Bgra8,
            payload: vec![10, 20, 30, 40, 50, 60, 70, 80],
        });

        let mut stream = encode_length_prefixed(&layout_msg).unwrap();
        stream.extend_from_slice(&encode_length_prefixed(&video_msg).unwrap());
        let mut cursor = stream.as_slice();

        match read_worker_message(&mut cursor).unwrap() {
            WorkerMessage::DisplayLayout(decoded) => assert_eq!(decoded, monitors),
            other => panic!("expected DisplayLayout, got {other:?}"),
        }
        match read_worker_message(&mut cursor).unwrap() {
            WorkerMessage::Frame(frame) => {
                assert_eq!(
                    (
                        frame.frame_index,
                        frame.display_id,
                        frame.width,
                        frame.height,
                        frame.stride,
                        frame.timestamp_us
                    ),
                    (7, 2, 2, 1, 8, 99)
                );
                assert_eq!(frame.data, vec![10, 20, 30, 40, 50, 60, 70, 80]);
            }
            other => panic!("expected Frame, got {other:?}"),
        }

        let unsupported = encode_length_prefixed(&WireMessage::Control(
            perigee_protocol::ControlMessage::KeepAlive,
        ))
        .unwrap();
        assert!(read_worker_message(&mut unsupported.as_slice()).is_err());
    }

    #[test]
    fn test_worker_capturer_forwards_display_layout_to_sink_and_enumerate_monitors() {
        let monitors = vec![
            MonitorInfo {
                monitor_id: 1,
                name: "DP-1".to_string(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale_factor: 1.0,
                refresh_rate_hz: 60,
                is_primary: true,
            },
            MonitorInfo {
                monitor_id: 2,
                name: "HDMI-1".to_string(),
                x: 1920,
                y: 0,
                width: 2560,
                height: 1440,
                scale_factor: 1.0,
                refresh_rate_hz: 60,
                is_primary: false,
            },
        ];
        let layout_msg = WireMessage::Topology(TopologyMessage::DisplayLayout {
            monitors: monitors.clone(),
        });
        let video_msg = WireMessage::Video(VideoFrameMessage {
            frame_id: 1,
            monitor_id: 2,
            timestamp_us: 123,
            codec: VideoCodec::RawBgra,
            is_keyframe: true,
            width: 2,
            height: 2,
            stride: 8,
            format: PixelFormat::Bgra8,
            payload: vec![42; 16],
        });

        let mut payload = encode_length_prefixed(&layout_msg).unwrap();
        payload.extend_from_slice(&encode_length_prefixed(&video_msg).unwrap());

        let temp_path = std::env::temp_dir().join(format!(
            "perigee-worker-layout-test-{}.bin",
            std::process::id()
        ));
        std::fs::write(&temp_path, &payload).unwrap();

        let received = Arc::new(Mutex::new(None::<(Vec<MonitorInfo>, u32)>));
        let received_sink = received.clone();

        let (loginctl, _calls) = FakeLoginctl::new();
        let script = format!(
            "cat '{}'; while IFS= read -r _; do :; done",
            temp_path.display()
        );
        let mut capturer =
            WorkerCapturer::with_loginctl(fake_worker_command(&script), 1, Box::new(loginctl))
                .with_layout_sink(Arc::new(move |layout, active_monitor_id| {
                    *received_sink.lock().unwrap() = Some((layout, active_monitor_id));
                }));

        let frame = capturer.capture_next_frame().unwrap();
        assert_eq!((frame.width, frame.height, frame.stride), (2, 2, 8));
        assert_eq!(frame.data, vec![42; 16]);
        assert_eq!(*received.lock().unwrap(), Some((monitors.clone(), 2)));
        assert_eq!(capturer.enumerate_monitors().unwrap(), monitors);

        drop(capturer);
        let _ = std::fs::remove_file(&temp_path);
    }
}
