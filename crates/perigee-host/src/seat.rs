//! Following logind's active session on a seat (greeter → user → greeter) by asking `loginctl`.
//!
//! The packaged host runs as the `perigee` user, so it only reads logind (unprivileged) and hands
//! the session's runtime dir to a capture worker that runs as that session's user.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The Wayland session logind reports as active on a seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    /// logind session id (`loginctl show-seat seat0 -p ActiveSession`).
    pub id: String,
    pub uid: u32,
    pub gid: u32,
    /// User name (`cosmic-greeter` at the login screen).
    pub user_name: String,
    /// logind session class: `greeter` or `user`.
    pub class: String,
    /// logind session type (always `wayland` here).
    pub session_type: String,
    /// The user's runtime dir (`/run/user/<uid>`), where the compositor socket lives.
    pub runtime_dir: PathBuf,
}

/// Runs `loginctl` with `args` and returns its stdout. A trait so tests can feed canned output.
pub trait LoginctlRunner {
    fn run(&self, args: &[&str]) -> Result<String>;
}

/// Runs the real `loginctl`.
pub struct SystemLoginctl;

impl LoginctlRunner for SystemLoginctl {
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("loginctl")
            .args(args)
            .output()
            .context("failed to run loginctl")?;
        if !output.status.success() {
            bail!(
                "loginctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("loginctl printed non-UTF-8 output")
    }
}

/// Returns the value of `key` in `KEY=VALUE` lines.
pub fn property<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// Returns the Wayland session active on `seat`, or `None` when the seat has no active session
/// or it is not a Wayland session (a text console, say).
pub fn query_active_session(
    runner: &dyn LoginctlRunner,
    seat: &str,
) -> Result<Option<ActiveSession>> {
    let seat_text = runner.run(&["show-seat", seat, "-p", "ActiveSession"])?;
    let Some(id) = property(&seat_text, "ActiveSession").filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let session = runner.run(&[
        "show-session",
        id,
        "-p",
        "User",
        "-p",
        "Name",
        "-p",
        "Class",
        "-p",
        "Type",
        "-p",
        "State",
    ])?;
    let session_type = property(&session, "Type").unwrap_or_default();
    let class = property(&session, "Class").unwrap_or_default();
    // greetd starts cosmic-greeter without XDG_SESSION_TYPE, so logind records the greeter as
    // tty/unspecified although it runs cosmic-comp. The worker finds (or fails to find) the
    // Wayland socket itself, so a greeter is always worth trying.
    if session_type != "wayland" && class != "greeter" {
        return Ok(None);
    }
    let uid: u32 = property(&session, "User")
        .context("logind session has no User")?
        .parse()
        .context("logind session User is not a uid")?;
    let user = runner.run(&[
        "show-user",
        &uid.to_string(),
        "-p",
        "GID",
        "-p",
        "RuntimePath",
    ])?;
    let gid: u32 = property(&user, "GID")
        .context("logind user has no GID")?
        .parse()
        .context("logind user GID is not a number")?;
    let runtime_dir = property(&user, "RuntimePath")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("logind user has no RuntimePath")?;
    Ok(Some(ActiveSession {
        id: id.to_string(),
        uid,
        gid,
        user_name: property(&session, "Name").unwrap_or_default().to_string(),
        class: class.to_string(),
        session_type: session_type.to_string(),
        runtime_dir,
    }))
}

/// Returns the compositor socket in `runtime_dir`: the lowest-numbered `wayland-N` entry that
/// is a Unix socket (lock files are skipped). Called by the capture worker, which runs as the
/// session's user, because the runtime dir is mode 0700.
pub fn find_wayland_socket(runtime_dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::FileTypeExt;

    let mut best: Option<(u32, PathBuf)> = None;
    let entries = std::fs::read_dir(runtime_dir)
        .with_context(|| format!("failed to list {}", runtime_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(number) = name
            .to_str()
            .and_then(|name| name.strip_prefix("wayland-"))
            .and_then(|number| number.parse::<u32>().ok())
        else {
            continue;
        };
        if !entry.file_type()?.is_socket() {
            continue;
        }
        if best.as_ref().is_none_or(|(lowest, _)| number < *lowest) {
            best = Some((number, entry.path()));
        }
    }
    best.map(|(_, path)| path)
        .with_context(|| format!("no wayland-N socket in {}", runtime_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeLoginctl(HashMap<String, String>);

    impl LoginctlRunner for FakeLoginctl {
        fn run(&self, args: &[&str]) -> Result<String> {
            self.0
                .get(&args[..2].join(" "))
                .cloned()
                .with_context(|| format!("unexpected loginctl {}", args.join(" ")))
        }
    }

    fn fake(pairs: &[(&str, &str)]) -> FakeLoginctl {
        FakeLoginctl(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    #[test]
    fn test_query_active_session_greeter_then_user() {
        let greeter = fake(&[
            ("show-seat seat0", "ActiveSession=c1\n"),
            (
                "show-session c1",
                "User=977\nName=cosmic-greeter\nClass=greeter\nType=wayland\nState=active\n",
            ),
            ("show-user 977", "GID=977\nRuntimePath=/run/user/977\n"),
        ]);
        assert_eq!(
            query_active_session(&greeter, "seat0").unwrap(),
            Some(ActiveSession {
                id: "c1".to_string(),
                uid: 977,
                gid: 977,
                user_name: "cosmic-greeter".to_string(),
                class: "greeter".to_string(),
                session_type: "wayland".to_string(),
                runtime_dir: PathBuf::from("/run/user/977"),
            })
        );

        let user = fake(&[
            ("show-seat seat0", "ActiveSession=4\n"),
            (
                "show-session 4",
                "User=1000\nName=testuser\nClass=user\nType=wayland\nState=active\n",
            ),
            ("show-user 1000", "GID=1000\nRuntimePath=/run/user/1000\n"),
        ]);
        let session = query_active_session(&user, "seat0").unwrap().unwrap();
        assert_eq!(
            (session.id.as_str(), session.uid, session.class.as_str()),
            ("4", 1000, "user")
        );
    }

    #[test]
    fn test_query_active_session_accepts_tty_greeter() {
        let greeter = fake(&[
            ("show-seat seat0", "ActiveSession=11\n"),
            (
                "show-session 11",
                "User=987\nName=cosmic-greeter\nClass=greeter\nType=tty\nState=active\n",
            ),
            ("show-user 987", "GID=987\nRuntimePath=/run/user/987\n"),
        ]);
        let session = query_active_session(&greeter, "seat0").unwrap().unwrap();
        assert_eq!(
            (
                session.id.as_str(),
                session.uid,
                session.class.as_str(),
                session.session_type.as_str()
            ),
            ("11", 987, "greeter", "tty")
        );
        assert_eq!(session.runtime_dir, PathBuf::from("/run/user/987"));
    }

    #[test]
    fn test_query_active_session_none_and_errors() {
        let empty = fake(&[("show-seat seat0", "ActiveSession=\n")]);
        assert_eq!(query_active_session(&empty, "seat0").unwrap(), None);
        let tty = fake(&[
            ("show-seat seat0", "ActiveSession=2\n"),
            (
                "show-session 2",
                "User=1000\nName=testuser\nClass=user\nType=tty\n",
            ),
        ]);
        assert_eq!(query_active_session(&tty, "seat0").unwrap(), None);
        let broken = fake(&[
            ("show-seat seat0", "ActiveSession=3\n"),
            ("show-session 3", "User=root\nType=wayland\n"),
        ]);
        assert!(query_active_session(&broken, "seat0").is_err());
    }

    #[test]
    fn test_find_wayland_socket_picks_lowest_socket() {
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("perigee-seat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_wayland_socket(&dir).is_err());
        std::fs::write(dir.join("wayland-0"), b"not a socket").unwrap();
        std::fs::write(dir.join("wayland-1.lock"), b"").unwrap();
        let _two = UnixListener::bind(dir.join("wayland-2")).unwrap();
        let _one = UnixListener::bind(dir.join("wayland-1")).unwrap();
        assert_eq!(find_wayland_socket(&dir).unwrap(), dir.join("wayland-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
