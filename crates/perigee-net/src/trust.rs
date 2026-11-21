//! Trusted node keys.
//!
//! A host keeps the keys of the clients allowed to connect; a client keeps the key it expects
//! from each host, labelled by connect target. Both use one file format, one entry per line:
//! `<64 hex chars> <label>`. Blank lines and lines starting with `#` are ignored. A malformed
//! line makes the whole file fail to load, so a damaged trust file never trusts anything by
//! accident.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, bail};

use crate::fsutil::write_private_file;
use crate::identity::{NODE_KEY_LEN, node_key_from_hex, node_key_to_hex};

/// File name of a host's trusted-client list inside the Perigee config directory.
pub const TRUSTED_CLIENTS_FILE_NAME: &str = "trusted_clients";

/// File name of a client's pinned host keys inside the Perigee config directory.
pub const KNOWN_HOSTS_FILE_NAME: &str = "known_hosts";

/// Decides whether a peer's node key may complete the TLS handshake.
pub trait PeerAuthorizer: Send + Sync + fmt::Debug {
    fn is_authorized(&self, node_key: &[u8; NODE_KEY_LEN]) -> bool;
}

/// One trusted node key and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub key: [u8; NODE_KEY_LEN],
    pub label: String,
}

/// In-memory authorizer for tests and embedded use.
#[derive(Debug, Default)]
pub struct AllowList {
    keys: RwLock<Vec<[u8; NODE_KEY_LEN]>>,
}

impl AllowList {
    pub fn new(keys: impl IntoIterator<Item = [u8; NODE_KEY_LEN]>) -> Self {
        Self {
            keys: RwLock::new(keys.into_iter().collect()),
        }
    }

    pub fn allow(&self, key: [u8; NODE_KEY_LEN]) {
        let mut keys = self.keys.write().unwrap_or_else(|p| p.into_inner());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
}

impl PeerAuthorizer for AllowList {
    fn is_authorized(&self, node_key: &[u8; NODE_KEY_LEN]) -> bool {
        self.keys
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(node_key)
    }
}

/// File-backed list of trusted node keys (mode 0600 on Unix, rewritten atomically).
#[derive(Debug)]
pub struct TrustedKeys {
    path: PathBuf,
    entries: RwLock<Vec<TrustedKey>>,
}

impl TrustedKeys {
    /// Loads the trust file at `path`; a missing file is an empty list.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let entries = if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read trust file {}", path.display()))?;
            parse_trusted_keys(&text)
                .with_context(|| format!("invalid trust file {}", path.display()))?
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            entries: RwLock::new(entries),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> Vec<TrustedKey> {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn contains(&self, key: &[u8; NODE_KEY_LEN]) -> bool {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|entry| &entry.key == key)
    }

    pub fn find_by_label(&self, label: &str) -> Option<TrustedKey> {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|entry| entry.label == label)
            .cloned()
    }

    /// Trusts `key` under `label`, replacing any entry with the same key or the same label,
    /// and rewrites the file.
    pub fn add(&self, key: [u8; NODE_KEY_LEN], label: &str) -> Result<()> {
        validate_label(label)?;
        let mut entries = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let mut next: Vec<TrustedKey> = entries
            .iter()
            .filter(|entry| entry.key != key && entry.label != label)
            .cloned()
            .collect();
        next.push(TrustedKey {
            key,
            label: label.to_string(),
        });
        self.save(&next)?;
        *entries = next;
        Ok(())
    }

    /// Pins `key` under `label`, replacing only an entry with the same label, and rewrites the
    /// file. One host key may be pinned under several labels (for example a name and an IP).
    pub fn pin(&self, key: [u8; NODE_KEY_LEN], label: &str) -> Result<()> {
        validate_label(label)?;
        let mut entries = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let mut next: Vec<TrustedKey> = entries
            .iter()
            .filter(|entry| entry.label != label)
            .cloned()
            .collect();
        next.push(TrustedKey {
            key,
            label: label.to_string(),
        });
        self.save(&next)?;
        *entries = next;
        Ok(())
    }

    /// Removes `key` and rewrites the file; returns whether it was present.
    pub fn remove(&self, key: &[u8; NODE_KEY_LEN]) -> Result<bool> {
        let mut entries = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let next: Vec<TrustedKey> = entries
            .iter()
            .filter(|entry| &entry.key != key)
            .cloned()
            .collect();
        if next.len() == entries.len() {
            return Ok(false);
        }
        self.save(&next)?;
        *entries = next;
        Ok(true)
    }

    /// Removes the entry labelled `label` and rewrites the file; returns whether it was present.
    pub fn remove_label(&self, label: &str) -> Result<bool> {
        let mut entries = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let next: Vec<TrustedKey> = entries
            .iter()
            .filter(|entry| entry.label != label)
            .cloned()
            .collect();
        if next.len() == entries.len() {
            return Ok(false);
        }
        self.save(&next)?;
        *entries = next;
        Ok(true)
    }

    fn save(&self, entries: &[TrustedKey]) -> Result<()> {
        let mut text = String::from("# Perigee trusted node keys: <64 hex chars> <label>\n");
        for entry in entries {
            text.push_str(&node_key_to_hex(&entry.key));
            text.push(' ');
            text.push_str(&entry.label);
            text.push('\n');
        }
        write_private_file(&self.path, text.as_bytes(), true)
    }
}

impl PeerAuthorizer for TrustedKeys {
    fn is_authorized(&self, node_key: &[u8; NODE_KEY_LEN]) -> bool {
        self.contains(node_key)
    }
}

fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() || label.chars().any(char::is_whitespace) {
        bail!("a trust label must be non-empty and contain no whitespace, got '{label}'");
    }
    Ok(())
}

fn parse_trusted_keys(text: &str) -> Result<Vec<TrustedKey>> {
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key_hex, label)) = line.split_once(char::is_whitespace) else {
            bail!("line {}: expected '<key> <label>'", index + 1);
        };
        let key = node_key_from_hex(key_hex).with_context(|| format!("line {}", index + 1))?;
        let label = label.trim();
        validate_label(label).with_context(|| format!("line {}", index + 1))?;
        entries.push(TrustedKey {
            key,
            label: label.to_string(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: [u8; NODE_KEY_LEN] = [0x11; NODE_KEY_LEN];
    const K2: [u8; NODE_KEY_LEN] = [0x22; NODE_KEY_LEN];

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("perigee-trust-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_trusted_keys_round_trip() {
        let dir = temp_dir("round-trip");
        let path = dir.join(KNOWN_HOSTS_FILE_NAME);
        let store = TrustedKeys::load(path.clone()).unwrap();
        assert!(store.entries().is_empty());
        store.add(K1, "laptop").unwrap();
        store.add(K2, "192.0.2.10:4433").unwrap();

        let reloaded = TrustedKeys::load(path.clone()).unwrap();
        assert_eq!(
            reloaded.entries(),
            vec![
                TrustedKey {
                    key: K1,
                    label: "laptop".to_string()
                },
                TrustedKey {
                    key: K2,
                    label: "192.0.2.10:4433".to_string()
                },
            ]
        );
        assert!(reloaded.contains(&K1));
        assert!(reloaded.is_authorized(&K2));
        assert!(!reloaded.is_authorized(&[0x33; NODE_KEY_LEN]));
        assert_eq!(
            reloaded
                .find_by_label("192.0.2.10:4433")
                .map(|entry| entry.key),
            Some(K2)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_replaces_same_key_or_label() {
        let dir = temp_dir("replace");
        let store = TrustedKeys::load(dir.join(TRUSTED_CLIENTS_FILE_NAME)).unwrap();
        store.add(K1, "a").unwrap();
        store.add(K1, "b").unwrap();
        assert_eq!(
            store.entries(),
            vec![TrustedKey {
                key: K1,
                label: "b".to_string()
            }]
        );
        store.add(K2, "b").unwrap();
        assert_eq!(
            store.entries(),
            vec![TrustedKey {
                key: K2,
                label: "b".to_string()
            }]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pin_keeps_one_key_under_many_labels() {
        let dir = temp_dir("pin");
        let store = TrustedKeys::load(dir.join(KNOWN_HOSTS_FILE_NAME)).unwrap();
        store.pin(K1, "host.example.com:4433").unwrap();
        store.pin(K1, "192.0.2.10:4433").unwrap();
        assert_eq!(store.entries().len(), 2);
        store.pin(K2, "192.0.2.10:4433").unwrap();
        assert_eq!(store.find_by_label("192.0.2.10:4433").unwrap().key, K2);
        assert_eq!(
            store.find_by_label("host.example.com:4433").unwrap().key,
            K1
        );
        assert!(store.remove_label("host.example.com:4433").unwrap());
        assert!(!store.remove_label("host.example.com:4433").unwrap());
        assert_eq!(store.entries().len(), 1);
        let reloaded = TrustedKeys::load(dir.join(KNOWN_HOSTS_FILE_NAME)).unwrap();
        assert_eq!(reloaded.entries(), store.entries());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_trusted_key() {
        let dir = temp_dir("remove");
        let store = TrustedKeys::load(dir.join(TRUSTED_CLIENTS_FILE_NAME)).unwrap();
        store.add(K1, "a").unwrap();
        assert!(store.remove(&K1).unwrap());
        assert!(!store.remove(&K1).unwrap());
        assert!(
            TrustedKeys::load(store.path().to_path_buf())
                .unwrap()
                .entries()
                .is_empty()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_malformed_trust_file_fails_closed() {
        let dir = temp_dir("malformed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TRUSTED_CLIENTS_FILE_NAME);
        fs::write(&path, "# comment\nzz notakey\n").unwrap();
        let err = TrustedKeys::load(path.clone()).unwrap_err();
        assert!(format!("{err:#}").contains("line 2"), "got: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_label_must_be_one_word() {
        let dir = temp_dir("label");
        let store = TrustedKeys::load(dir.join(KNOWN_HOSTS_FILE_NAME)).unwrap();
        assert!(store.add(K1, "two words").is_err());
        assert!(store.add(K1, "").is_err());
        assert!(store.entries().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_allow_list() {
        let list = AllowList::new([K1]);
        assert!(list.is_authorized(&K1));
        assert!(!list.is_authorized(&K2));
        list.allow(K2);
        assert!(list.is_authorized(&K2));
    }
}
