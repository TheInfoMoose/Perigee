//! Private-file writes shared by the identity and trust stores.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` with mode 0600 (Unix) so no reader ever sees a partial file: the
/// bytes go to a unique temp file in the same directory, which is synced and closed, then
/// renamed over `path` (`replace`) or hard-linked to it (`!replace`, which fails if `path`
/// already exists). Parent directories are created. The temp file never outlives the call.
pub(crate) fn write_private_file(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
        .to_string_lossy();
    let tmp = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write_synced(&tmp, bytes).and_then(|()| {
        if replace {
            fs::rename(&tmp, path).with_context(|| format!("failed to replace {}", path.display()))
        } else {
            fs::hard_link(&tmp, path)
                .with_context(|| format!("failed to create {}", path.display()))
        }
    });
    let _ = fs::remove_file(&tmp);
    result
}

fn write_synced(tmp: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", tmp.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("perigee-fsutil-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_write_private_file_replace_and_no_clobber() {
        let dir = temp_dir("write");
        let path = dir.join("nested").join("file");
        write_private_file(&path, b"one", false).unwrap();
        assert!(write_private_file(&path, b"two", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"one");
        write_private_file(&path, b"three", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"three");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
