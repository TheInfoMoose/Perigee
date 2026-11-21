//! [`FrameCapturer`] that follows logind's active session on seat0 by running
//! `capture-worker` as that session's user and reading raw frames from its stdout.
//!
//! The parent keeps `CAP_SETUID`/`CAP_SETGID` only to start the worker; the worker gets the
//! session's uid and gid, no supplementary groups and no ambient capabilities.

use std::io::{BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use perigee_protocol::{
    MAX_WIRE_FRAME_BYTES, MonitorInfo, PixelFormat, WireMessage, decode_wire_message,
};

use crate::backends::linux_capture::FrameCapturer;
use crate::seat::{ActiveSession, LoginctlRunner, SystemLoginctl, query_active_session};
use crate::{CaptureBackendType, CapturedFrame};

/// How often the capturer re-reads logind's active session.
const SEAT_POLL_INTERVAL: Duration = Duration::from_secs(1);

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

    /// Builds the worker process for `session`: its uid and gid (when they differ from ours),
    /// no supplementary groups, no ambient capabilities, a cleared environment with
    /// `XDG_RUNTIME_DIR` set, and stdout piped for frames.
    pub fn build(&self, session: &ActiveSession, output: usize) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.prefix_args)
            .arg("capture-worker")
            .arg("--output")
            .arg(output.to_string())
            .env_clear()
            .env("XDG_RUNTIME_DIR", &session.runtime_dir)
            .env("HOME", "/")
            .env(
                "RUST_LOG",
                std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            )
            .stdin(Stdio::null())
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

/// Reads one length-prefixed `WireMessage::Video` raw frame from a capture worker.
pub fn read_worker_frame(reader: &mut impl Read) -> Result<CapturedFrame> {
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
    let WireMessage::Video(msg) = decode_wire_message(&body)? else {
        bail!("capture worker sent a non-video message");
    };
    if !matches!(msg.format, PixelFormat::Bgra8 | PixelFormat::Rgba8) {
        bail!("capture worker sent a {:?} frame", msg.format);
    }
    Ok(CapturedFrame {
        frame_index: msg.frame_id,
        timestamp_us: msg.timestamp_us,
        display_id: msg.monitor_id,
        width: msg.width,
        height: msg.height,
        stride: msg.stride,
        pixel_format: msg.format,
        data: msg.payload,
        is_dmabuf_or_metal_handle: false,
    })
}

/// Captures whichever Wayland session logind reports active on seat0, through a
/// `capture-worker` child running as that session's user. Restarts the worker when the active
/// session changes or the worker exits (at most once per poll); while no Wayland session is
/// active it repeats the last frame (black 64x64 before the first) so the remote session stays
/// open across greeter ↔ user switches.
pub struct WorkerCapturer {
    command: WorkerCommand,
    output: usize,
    seat: String,
    loginctl: Box<dyn LoginctlRunner + Send + Sync>,
    current: Option<(ActiveSession, Child, BufReader<ChildStdout>)>,
    last_poll: Option<Instant>,
    last_frame: CapturedFrame,
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
            seat: "seat0".to_string(),
            loginctl,
            current: None,
            last_poll: None,
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
            frame_counter: 0,
            start: Instant::now(),
        }
    }

    fn poll_seat(&mut self) -> Result<()> {
        self.last_poll = Some(Instant::now());
        let active = query_active_session(self.loginctl.as_ref(), &self.seat)?;
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
            info!(
                session = %session.id,
                user = %session.user_name,
                class = %session.class,
                "Starting capture worker for the active session"
            );
            let mut child = self
                .command
                .build(&session, self.output)
                .spawn()
                .context("failed to start the capture worker")?;
            let stdout = child
                .stdout
                .take()
                .context("capture worker has no stdout")?;
            self.current = Some((session, child, BufReader::new(stdout)));
        }
        Ok(())
    }

    fn stop_worker(&mut self) {
        if let Some((session, mut child, _)) = self.current.take() {
            let _ = child.kill();
            let _ = child.wait();
            info!(session = %session.id, "Stopped capture worker");
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
        if self
            .last_poll
            .is_none_or(|at| at.elapsed() >= SEAT_POLL_INTERVAL)
        {
            self.poll_seat()?;
        }
        let read = match self.current.as_mut() {
            None => None,
            Some((session, _, reader)) => Some((session.id.clone(), read_worker_frame(reader))),
        };
        match read {
            None => {
                std::thread::sleep(Duration::from_millis(100));
                Ok(self.repeat_last_frame())
            }
            Some((_, Ok(mut frame))) => {
                self.frame_counter += 1;
                frame.frame_index = self.frame_counter;
                self.last_frame = frame.clone();
                Ok(frame)
            }
            Some((session_id, Err(e))) => {
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
        assert_eq!(args, ["host", "capture-worker", "--output", "0"]);
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
}
