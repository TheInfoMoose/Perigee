//! Privilege-separated Linux capture broker over a Unix domain socket with `SCM_RIGHTS` FD passing.
//!
//! The broker validates the active `seat0` Wayland/greeter session via `logind`, refuses root
//! sessions (`uid == 0 || gid == 0`), spawns `capture-worker` as that session's user, transfers
//! the worker's `stdin` lifeline and `stdout` frame pipe FDs to `perigee-host`, and retains
//! ownership of the `Child` process so it can reap or kill it when the client disconnects.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tracing::{info, warn};

use crate::backends::wayland_dmabuf::{CaptureMode, parse_capture_mode};
use crate::backends::worker::{WorkerCommand, wait_for_worker_exit};
use crate::seat::{ActiveSession, LoginctlRunner, SystemLoginctl, query_active_session};

/// Default filesystem path for the Linux `capture-broker` Unix domain socket.
pub const DEFAULT_BROKER_SOCKET_PATH: &str = "/run/perigee/capture-broker.sock";

/// Maximum number of concurrent capture workers managed by a single `CaptureBroker`.
pub const MAX_BROKER_CONCURRENT_WORKERS: usize = 4;

/// Maximum length of a single line-protocol request accepted by `CaptureBroker`.
const MAX_REQUEST_LINE_BYTES: usize = 256;

/// Read timeout enforced while waiting for a client's initial `SPAWN` request line.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum reply byte length accepted by `request_broker_worker`.
const MAX_REPLY_BYTES: usize = 512;

/// Client request asking `capture-broker` to spawn a worker for the active seat0 session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerSpawnRequest {
    pub output: usize,
    pub capture_mode: CaptureMode,
}

/// Formats a [`BrokerSpawnRequest`] as `"SPAWN <output> <auto|dmabuf|shm>\n"`.
pub fn format_spawn_request(req: &BrokerSpawnRequest) -> String {
    let mode = match req.capture_mode {
        CaptureMode::Auto => "auto",
        CaptureMode::Dmabuf => "dmabuf",
        CaptureMode::Shm => "shm",
    };
    format!("SPAWN {} {mode}\n", req.output)
}

/// Parses a `"SPAWN <output> <mode>"` line request (trimmed), rejecting unknown verbs, wrong
/// token counts, invalid `usize` output indices, or invalid [`CaptureMode`] values.
pub fn parse_spawn_request(line: &str) -> Result<BrokerSpawnRequest> {
    let trimmed = line.trim();
    let mut parts = trimmed.split_whitespace();
    let verb = parts.next().context("empty broker request")?;
    if verb != "SPAWN" {
        bail!("unsupported broker request verb '{verb}'");
    }
    let output_str = parts
        .next()
        .context("broker SPAWN request missing output index")?;
    let mode_str = parts
        .next()
        .context("broker SPAWN request missing capture mode")?;
    if parts.next().is_some() {
        bail!("broker SPAWN request has unexpected trailing tokens");
    }
    let output = output_str
        .parse::<usize>()
        .with_context(|| format!("invalid broker output index '{output_str}'"))?;
    let capture_mode = parse_capture_mode(mode_str)?;
    Ok(BrokerSpawnRequest {
        output,
        capture_mode,
    })
}

/// Rejects root sessions (`uid == 0 || gid == 0`) so `capture-broker` never spawns a root worker.
pub fn validate_non_root_session(session: &ActiveSession) -> Result<()> {
    if session.uid == 0 || session.gid == 0 {
        bail!(
            "refusing to spawn capture worker for root session '{}' (uid={}, gid={})",
            session.id,
            session.uid,
            session.gid
        );
    }
    Ok(())
}

/// Sends `message` along with `fds` over `stream` using `sendmsg(2)` and `SCM_RIGHTS`.
pub fn send_response_with_fds(stream: &UnixStream, message: &[u8], fds: &[RawFd]) -> Result<()> {
    if message.is_empty() {
        bail!("cannot send empty broker message over Unix socket");
    }
    let fds_bytes = std::mem::size_of_val(fds);
    let fds_bytes_u32 = u32::try_from(fds_bytes).context("too many file descriptors to send")?;

    let (cmsg_space, cmsg_len_val) = if !fds.is_empty() {
        // SAFETY: CMSG_SPACE and CMSG_LEN are pure size-calculation macros/functions over u32.
        unsafe {
            (
                libc::CMSG_SPACE(fds_bytes_u32) as usize,
                libc::CMSG_LEN(fds_bytes_u32),
            )
        }
    } else {
        (0, 0)
    };

    let cmsg_elem_size = std::mem::size_of::<libc::cmsghdr>();
    let mut cmsg_buf: Vec<libc::cmsghdr> = if cmsg_space > 0 {
        // SAFETY: libc::cmsghdr is a plain-old-data C struct that is valid when zero-initialized.
        vec![unsafe { std::mem::zeroed() }; cmsg_space.div_ceil(cmsg_elem_size)]
    } else {
        Vec::new()
    };

    let mut sent = 0usize;
    while sent < message.len() {
        let chunk = &message[sent..];
        let mut iov = libc::iovec {
            iov_base: chunk.as_ptr() as *mut libc::c_void,
            iov_len: chunk.len(),
        };
        // SAFETY: libc::msghdr is a plain-old-data C struct that is valid when zero-initialized.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        if sent == 0 && !fds.is_empty() {
            msg.msg_control = cmsg_buf.as_mut_ptr().cast::<libc::c_void>();
            msg.msg_controllen = cmsg_space as _;
            // SAFETY: msg.msg_control points to a zeroed, cmsghdr-aligned buffer of cmsg_space bytes.
            let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
            if cmsg.is_null() {
                bail!("failed to construct SCM_RIGHTS control header");
            }
            // SAFETY: cmsg is non-null and within cmsg_buf with capacity for cmsg_len_val bytes;
            // fds is valid for fds_bytes bytes and does not overlap cmsg_buf.
            unsafe {
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = cmsg_len_val as _;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(cmsg),
                    fds_bytes,
                );
            }
        }

        // SAFETY: stream holds a valid open Unix socket FD, and msg/iov/cmsg_buf remain live and
        // valid for the duration of the sendmsg call.
        let rc = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("sendmsg failed on broker Unix socket");
        }
        if rc == 0 {
            bail!("peer closed broker Unix socket during sendmsg");
        }
        sent += rc as usize;
    }

    Ok(())
}

/// Receives up to `max_bytes` of payload and up to `max_fds` file descriptors via `recvmsg(2)`
/// with `SCM_RIGHTS`, rejecting truncated control data (`MSG_CTRUNC`).
pub fn recv_response_with_fds(
    stream: &UnixStream,
    max_bytes: usize,
    max_fds: usize,
) -> Result<(Vec<u8>, Vec<OwnedFd>)> {
    if max_bytes == 0 {
        bail!("max_bytes must be greater than zero");
    }
    let mut data_buf = vec![0u8; max_bytes];
    let max_fds_bytes = max_fds
        .checked_mul(std::mem::size_of::<RawFd>())
        .context("max_fds byte size overflow")?;
    let max_fds_bytes_u32 =
        u32::try_from(max_fds_bytes).context("max_fds exceeds u32 control buffer limit")?;

    let cmsg_space = if max_fds > 0 {
        // SAFETY: CMSG_SPACE is a pure size calculation over a u32 payload length.
        unsafe { libc::CMSG_SPACE(max_fds_bytes_u32) as usize }
    } else {
        0
    };

    let cmsg_elem_size = std::mem::size_of::<libc::cmsghdr>();
    let mut cmsg_buf: Vec<libc::cmsghdr> = if cmsg_space > 0 {
        // SAFETY: libc::cmsghdr is a plain-old-data C struct that is valid when zero-initialized.
        vec![unsafe { std::mem::zeroed() }; cmsg_space.div_ceil(cmsg_elem_size)]
    } else {
        Vec::new()
    };

    let mut iov = libc::iovec {
        iov_base: data_buf.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: data_buf.len(),
    };

    // SAFETY: libc::msghdr is a plain-old-data C struct that is valid when zero-initialized.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    let received_len = loop {
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if cmsg_space > 0 {
            msg.msg_control = cmsg_buf.as_mut_ptr().cast::<libc::c_void>();
            msg.msg_controllen = cmsg_space as _;
        } else {
            msg.msg_control = std::ptr::null_mut();
            msg.msg_controllen = 0;
        }
        msg.msg_flags = 0;

        // SAFETY: stream holds a valid open Unix socket FD, and data_buf/cmsg_buf remain live
        // and exclusively borrowed for the duration of the recvmsg call.
        let rc = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("recvmsg failed on broker Unix socket");
        }
        break rc as usize;
    };

    // Always collect delivered FDs into OwnedFd first so any early error (including MSG_CTRUNC)
    // automatically closes them on drop instead of leaking descriptors.
    let mut received_fds = Vec::new();
    if cmsg_space > 0 {
        // SAFETY: msg was populated by recvmsg with a valid cmsghdr-aligned cmsg_buf.
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !cmsg.is_null() {
            // SAFETY: cmsg is non-null and points to a valid cmsghdr within cmsg_buf.
            let (level, ty, len) = unsafe {
                (
                    (*cmsg).cmsg_level,
                    (*cmsg).cmsg_type,
                    (*cmsg).cmsg_len as usize,
                )
            };
            if level == libc::SOL_SOCKET && ty == libc::SCM_RIGHTS {
                // SAFETY: CMSG_LEN(0) is a pure size calculation for the cmsghdr header prefix.
                let hdr_len = unsafe { libc::CMSG_LEN(0) } as usize;
                if len >= hdr_len {
                    let payload_len = len - hdr_len;
                    let fd_count = payload_len / std::mem::size_of::<RawFd>();
                    // SAFETY: CMSG_DATA(cmsg) points to payload_len bytes within cmsg_buf.
                    let data_ptr = unsafe { libc::CMSG_DATA(cmsg) };
                    for idx in 0..fd_count {
                        let mut raw_fd: RawFd = -1;
                        // SAFETY: idx * size_of::<RawFd>() + size_of::<RawFd>() <= payload_len,
                        // so reading from data_ptr is in bounds of cmsg_buf.
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                data_ptr.add(idx * std::mem::size_of::<RawFd>()),
                                (&mut raw_fd as *mut RawFd).cast::<u8>(),
                                std::mem::size_of::<RawFd>(),
                            );
                        }
                        if raw_fd >= 0 {
                            // SAFETY: raw_fd was newly installed in this process by recvmsg(SCM_RIGHTS)
                            // with MSG_CMSG_CLOEXEC and is uniquely owned here.
                            received_fds.push(unsafe { OwnedFd::from_raw_fd(raw_fd) });
                        }
                    }
                }
            }
            // SAFETY: cmsg is a valid control header pointer within msg's control buffer.
            cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
        }
    }

    if (msg.msg_flags & libc::MSG_CTRUNC) != 0 {
        bail!("SCM_RIGHTS control data truncated (MSG_CTRUNC)");
    }

    data_buf.truncate(received_len);
    Ok((data_buf, received_fds))
}

/// Worker pipe handles and control connection returned by [`request_broker_worker`].
///
/// Dropping `stdin` signals the worker to exit politely; dropping `control` signals the broker
/// to wait for and reap (or kill) the worker child process.
#[derive(Debug)]
pub struct BrokerSpawnedWorker {
    pub session_id: String,
    pub stdin: File,
    pub stdout: File,
    pub control: UnixStream,
}

/// Connects to `capture-broker` at `socket_path` and requests a capture worker for the active
/// `seat0` session with the given `output` index and `capture_mode`.
///
/// Returns `Ok(None)` when no Wayland/greeter session is active on `seat0`.
pub fn request_broker_worker(
    socket_path: &Path,
    output: usize,
    capture_mode: CaptureMode,
) -> Result<Option<BrokerSpawnedWorker>> {
    let mut stream = UnixStream::connect(socket_path).with_context(|| {
        format!(
            "failed to connect to capture-broker at {}",
            socket_path.display()
        )
    })?;
    let request = format_spawn_request(&BrokerSpawnRequest {
        output,
        capture_mode,
    });
    stream
        .write_all(request.as_bytes())
        .context("failed to write SPAWN request to capture-broker")?;
    stream
        .flush()
        .context("failed to flush SPAWN request to capture-broker")?;

    let (reply_bytes, fds) = recv_response_with_fds(&stream, MAX_REPLY_BYTES, 4)?;
    let reply =
        String::from_utf8(reply_bytes).context("capture-broker returned non-UTF-8 reply")?;

    if reply == "NONE\n" {
        if !fds.is_empty() {
            bail!(
                "capture-broker replied NONE with {} unexpected file descriptors",
                fds.len()
            );
        }
        return Ok(None);
    }

    if let Some(rest) = reply.strip_prefix("OK ") {
        let session_id = rest
            .strip_suffix('\n')
            .context("malformed capture-broker OK reply (missing trailing newline)")?;
        if session_id.is_empty() || session_id.chars().any(char::is_whitespace) {
            bail!("malformed capture-broker OK reply: invalid session id '{session_id}'");
        }
        let [stdin_fd, stdout_fd]: [OwnedFd; 2] = fds.try_into().map_err(|fds: Vec<OwnedFd>| {
            anyhow!(
                "capture-broker replied OK with {} file descriptors (expected 2)",
                fds.len()
            )
        })?;
        return Ok(Some(BrokerSpawnedWorker {
            session_id: session_id.to_string(),
            stdin: File::from(stdin_fd),
            stdout: File::from(stdout_fd),
            control: stream,
        }));
    }

    if let Some(rest) = reply.strip_prefix("ERR ") {
        let reason = rest.trim_end_matches('\n');
        bail!("capture-broker error: {reason}");
    }

    bail!("malformed capture-broker reply: {reply:?}");
}

struct ActiveWorkerGuard {
    counter: Arc<AtomicUsize>,
}

impl ActiveWorkerGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= max {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        counter: Arc::clone(counter),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for ActiveWorkerGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

fn read_bounded_line(stream: &mut UnixStream, max_bytes: usize) -> Result<String> {
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    bail!("client disconnected before sending request");
                }
                break;
            }
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                if buf.len() > max_bytes {
                    bail!("broker request exceeds {max_bytes}-byte limit");
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("failed to read broker request"),
        }
    }
    String::from_utf8(buf).context("broker request is not valid UTF-8")
}

fn send_error_reply(stream: &UnixStream, message: &str) {
    let sanitized = message.replace(['\r', '\n'], " ");
    let reply = format!("ERR {sanitized}\n");
    let _ = send_response_with_fds(stream, reply.as_bytes(), &[]);
}

/// Privileged, network-isolated Linux capture broker that spawns `capture-worker` for the active
/// non-root `seat0` session and transfers its stdio pipes over a Unix domain socket.
pub struct CaptureBroker {
    command: WorkerCommand,
    seat: String,
    loginctl: Arc<dyn LoginctlRunner + Send + Sync>,
    active_workers: Arc<AtomicUsize>,
}

impl CaptureBroker {
    /// Creates a `CaptureBroker` targeting `seat0` via the real `loginctl` binary.
    pub fn new(command: WorkerCommand) -> Self {
        Self::with_loginctl(command, "seat0", Arc::new(SystemLoginctl))
    }

    /// Creates a `CaptureBroker` with an explicit seat name and [`LoginctlRunner`] implementation.
    pub fn with_loginctl(
        command: WorkerCommand,
        seat: impl Into<String>,
        loginctl: Arc<dyn LoginctlRunner + Send + Sync>,
    ) -> Self {
        Self {
            command,
            seat: seat.into(),
            loginctl,
            active_workers: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Handles a single client connection: validates the `SPAWN` request and active non-root
    /// session, spawns `capture-worker`, transfers its `stdin`/`stdout` FDs via `SCM_RIGHTS`,
    /// waits for the client to disconnect, and reaps/kills the child before releasing the slot.
    pub fn handle_connection(&self, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
        let line = match read_bounded_line(&mut stream, MAX_REQUEST_LINE_BYTES) {
            Ok(line) => line,
            Err(err) => {
                send_error_reply(&stream, &format!("{err:#}"));
                return;
            }
        };
        let req = match parse_spawn_request(&line) {
            Ok(req) => req,
            Err(err) => {
                send_error_reply(&stream, &format!("{err:#}"));
                return;
            }
        };

        let Some(_worker_guard) =
            ActiveWorkerGuard::try_acquire(&self.active_workers, MAX_BROKER_CONCURRENT_WORKERS)
        else {
            send_error_reply(&stream, "too many active workers");
            return;
        };

        let session = match query_active_session(self.loginctl.as_ref(), &self.seat) {
            Ok(Some(session)) => session,
            Ok(None) => {
                let _ = send_response_with_fds(&stream, b"NONE\n", &[]);
                return;
            }
            Err(err) => {
                send_error_reply(&stream, &format!("failed to query active session: {err:#}"));
                return;
            }
        };

        if let Err(err) = validate_non_root_session(&session) {
            warn!(
                session = %session.id,
                uid = session.uid,
                gid = session.gid,
                error = %err,
                "Capture broker refused root session"
            );
            send_error_reply(&stream, &format!("{err:#}"));
            return;
        }

        info!(
            session = %session.id,
            user = %session.user_name,
            class = %session.class,
            output = req.output,
            "Capture broker spawning capture worker"
        );

        let mut child = match self
            .command
            .build_with_mode(&session, req.output, req.capture_mode)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                send_error_reply(&stream, &format!("failed to spawn capture worker: {err}"));
                return;
            }
        };

        let Some(stdin) = child.stdin.take() else {
            wait_for_worker_exit(&session, child);
            send_error_reply(&stream, "capture worker has no stdin");
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            wait_for_worker_exit(&session, child);
            send_error_reply(&stream, "capture worker has no stdout");
            return;
        };

        let reply = format!("OK {}\n", session.id);
        let send_result = send_response_with_fds(
            &stream,
            reply.as_bytes(),
            &[stdin.as_raw_fd(), stdout.as_raw_fd()],
        );
        // Drop the broker's copies of stdin/stdout immediately so the client holds the sole
        // lifeline write end (or, on send error, so the worker sees stdin EOF and exits).
        drop(stdin);
        drop(stdout);

        if let Err(err) = send_result {
            warn!(
                session = %session.id,
                error = %err,
                "Failed to transfer capture worker FDs to client; stopping worker"
            );
            wait_for_worker_exit(&session, child);
            return;
        }

        // Wait until the client disconnects or drops its control stream before reaping the worker.
        let _ = stream.set_read_timeout(None);
        let mut drain = [0u8; 64];
        loop {
            match stream.read(&mut drain) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        wait_for_worker_exit(&session, child);
    }

    /// Binds the broker Unix domain socket at `socket_path` (mode `0660`), or adopts FD 3 when
    /// systemd socket activation (`LISTEN_FDS=1` and matching `LISTEN_PID`) is active.
    pub fn bind_broker_listener(socket_path: &Path) -> Result<UnixListener> {
        let listen_fds = std::env::var("LISTEN_FDS").ok();
        let listen_pid = std::env::var("LISTEN_PID").ok();
        if listen_fds.as_deref() == Some("1")
            && listen_pid
                .as_deref()
                .and_then(|pid| pid.parse::<u32>().ok())
                == Some(std::process::id())
        {
            info!("Adopting systemd-activated capture-broker Unix socket (FD 3)");
            // SAFETY: systemd socket activation guarantees FD 3 is an open, listening Unix
            // domain socket exclusively inherited by this process when LISTEN_FDS=1 and
            // LISTEN_PID matches our PID.
            let listener = unsafe { UnixListener::from_raw_fd(3) };
            return Ok(listener);
        }

        if let Some(parent) = socket_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create broker socket directory {}",
                    parent.display()
                )
            })?;
        }
        if socket_path.symlink_metadata().is_ok() {
            std::fs::remove_file(socket_path).with_context(|| {
                format!(
                    "failed to remove stale broker socket {}",
                    socket_path.display()
                )
            })?;
        }
        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("failed to bind broker socket {}", socket_path.display()))?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))
            .with_context(|| {
                format!(
                    "failed to set permissions 0660 on broker socket {}",
                    socket_path.display()
                )
            })?;
        Ok(listener)
    }

    /// Accepts connections on `listener` in a loop and spawns a worker thread per connection.
    pub fn serve_listener(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let broker = Arc::clone(&self);
                    std::thread::spawn(move || {
                        broker.handle_connection(stream);
                    });
                }
                Err(err) => {
                    warn!(error = %err, "Failed to accept capture-broker connection");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    static TEST_SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_socket_path(label: &str) -> PathBuf {
        let seq = TEST_SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "perigee-broker-{label}-{}-{seq}.sock",
            std::process::id()
        ))
    }

    struct CannedLoginctl {
        session_id: Option<String>,
        uid: u32,
        gid: u32,
        runtime_dir: PathBuf,
    }

    impl LoginctlRunner for CannedLoginctl {
        fn run(&self, args: &[&str]) -> Result<String> {
            match args.first().copied() {
                Some("show-seat") => match &self.session_id {
                    Some(id) => Ok(format!("ActiveSession={id}\n")),
                    None => Ok("ActiveSession=\n".to_string()),
                },
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
    fn test_broker_protocol_parse_and_format_roundtrip() {
        for (output, mode, expected_line) in [
            (0, CaptureMode::Auto, "SPAWN 0 auto\n"),
            (1, CaptureMode::Dmabuf, "SPAWN 1 dmabuf\n"),
            (2, CaptureMode::Shm, "SPAWN 2 shm\n"),
        ] {
            let req = BrokerSpawnRequest {
                output,
                capture_mode: mode,
            };
            let formatted = format_spawn_request(&req);
            assert_eq!(formatted, expected_line);
            assert_eq!(parse_spawn_request(&formatted).unwrap(), req);
        }

        for invalid in [
            "",
            "   \n",
            "EXEC 0 auto\n",
            "SPAWN 0\n",
            "SPAWN 0 auto extra\n",
            "SPAWN -1 auto\n",
            "SPAWN abc auto\n",
            "SPAWN 0 unknown\n",
        ] {
            assert!(
                parse_spawn_request(invalid).is_err(),
                "expected parse failure for {invalid:?}"
            );
        }
    }

    #[test]
    fn test_broker_rejects_root_session() {
        let non_root = ActiveSession {
            id: "c1".to_string(),
            uid: 1000,
            gid: 1000,
            user_name: "testuser".to_string(),
            class: "user".to_string(),
            session_type: "wayland".to_string(),
            runtime_dir: PathBuf::from("/run/user/1000"),
        };
        assert!(validate_non_root_session(&non_root).is_ok());

        let root_uid = ActiveSession {
            uid: 0,
            ..non_root.clone()
        };
        let err = validate_non_root_session(&root_uid).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("root") && msg.contains("uid=0"), "got: {msg}");

        let root_gid = ActiveSession { gid: 0, ..non_root };
        let err = validate_non_root_session(&root_gid).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("root") && msg.contains("gid=0"), "got: {msg}");

        let socket_path = temp_socket_path("reject-root");
        let listener = CaptureBroker::bind_broker_listener(&socket_path).unwrap();
        let marker = socket_path.with_extension("spawned");
        let _ = std::fs::remove_file(&marker);
        let broker = Arc::new(CaptureBroker::with_loginctl(
            fake_worker_command(&format!("touch '{}'", marker.display())),
            "seat0",
            Arc::new(CannedLoginctl {
                session_id: Some("c0".to_string()),
                uid: 0,
                gid: 1000,
                runtime_dir: std::env::temp_dir(),
            }),
        ));

        let server_broker = Arc::clone(&broker);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_broker.handle_connection(stream);
        });

        let err = request_broker_worker(&socket_path, 0, CaptureMode::Auto).unwrap_err();
        server.join().unwrap();
        let _ = std::fs::remove_file(&socket_path);

        let err_msg = format!("{err:#}");
        assert!(
            err_msg.contains("root"),
            "expected root rejection error, got: {err_msg}"
        );
        assert!(
            !marker.exists(),
            "broker should not have spawned a child for a root session"
        );
    }

    #[test]
    fn test_broker_roundtrip_passes_fds_and_reaps_worker_on_disconnect() {
        // SAFETY: geteuid and getegid only read the calling process's credentials; they cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        assert!(
            uid != 0 && gid != 0,
            "test_broker_roundtrip_passes_fds_and_reaps_worker_on_disconnect requires non-root uid/gid"
        );

        let socket_path = temp_socket_path("roundtrip");
        let listener = CaptureBroker::bind_broker_listener(&socket_path).unwrap();
        let broker = Arc::new(CaptureBroker::with_loginctl(
            fake_worker_command("printf 'ready\\n'; while IFS= read -r _; do :; done"),
            "seat0",
            Arc::new(CannedLoginctl {
                session_id: Some("c7".to_string()),
                uid,
                gid,
                runtime_dir: std::env::temp_dir(),
            }),
        ));

        let server_broker = Arc::clone(&broker);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_broker.handle_connection(stream);
        });

        let worker = request_broker_worker(&socket_path, 0, CaptureMode::Auto)
            .unwrap()
            .expect("expected active session worker");
        assert_eq!(worker.session_id, "c7");

        let mut reader = BufReader::new(&worker.stdout);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "ready\n");

        let started = Instant::now();
        drop(reader);
        drop(worker);

        server.join().unwrap();
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&socket_path);

        assert!(
            elapsed < Duration::from_secs(1),
            "broker should reap worker within 1s of disconnect, took {elapsed:?}"
        );
    }

    #[test]
    fn test_broker_returns_none_when_no_active_session() {
        let socket_path = temp_socket_path("no-session");
        let listener = CaptureBroker::bind_broker_listener(&socket_path).unwrap();
        let broker = Arc::new(CaptureBroker::with_loginctl(
            fake_worker_command("exit 1"),
            "seat0",
            Arc::new(CannedLoginctl {
                session_id: None,
                uid: 1000,
                gid: 1000,
                runtime_dir: std::env::temp_dir(),
            }),
        ));

        let server_broker = Arc::clone(&broker);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_broker.handle_connection(stream);
        });

        let result = request_broker_worker(&socket_path, 0, CaptureMode::Shm).unwrap();
        server.join().unwrap();
        let _ = std::fs::remove_file(&socket_path);

        assert!(result.is_none());
    }
}
