//! Persistent Ed25519 node identity for mutual TLS.
//!
//! Every Perigee node (host or client) owns one Ed25519 key. Its 32-byte public key is the
//! node's identity: peers pin it, and the TLS certificate each side presents is self-signed
//! with it, so completing the TLS 1.3 handshake proves possession of the pinned key.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use zeroize::{Zeroize, Zeroizing};

use crate::fsutil::write_private_file;

/// Length of an Ed25519 public key (a node key) in bytes.
pub const NODE_KEY_LEN: usize = 32;

/// TLS server name every Perigee certificate is issued for.
pub const PERIGEE_SERVER_NAME: &str = "perigee.host";

/// File name of the node identity inside the Perigee config directory.
pub const IDENTITY_FILE_NAME: &str = "identity.key";

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo` (RFC 8410); the 32 key bytes follow it.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// A node's Ed25519 key pair. The private key is zeroized on drop and never printed.
pub struct NodeIdentity {
    pkcs8_der: Vec<u8>,
    public_key: [u8; NODE_KEY_LEN],
}

impl NodeIdentity {
    /// Generates a fresh Ed25519 node key.
    pub fn generate() -> Result<Self> {
        let key_pair =
            KeyPair::generate_for(&PKCS_ED25519).context("failed to generate Ed25519 node key")?;
        Self::from_key_pair(&key_pair)
    }

    /// Loads a node key from PKCS#8 DER bytes.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self> {
        let pkcs8 = PrivatePkcs8KeyDer::from(der.to_vec());
        let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &PKCS_ED25519)
            .context("node identity is not a valid Ed25519 PKCS#8 key")?;
        Self::from_key_pair(&key_pair)
    }

    fn from_key_pair(key_pair: &KeyPair) -> Result<Self> {
        let public_key: [u8; NODE_KEY_LEN] = key_pair
            .public_key_raw()
            .try_into()
            .map_err(|_| anyhow!("Ed25519 public key is not {NODE_KEY_LEN} bytes"))?;
        Ok(Self {
            pkcs8_der: key_pair.serialize_der(),
            public_key,
        })
    }

    /// Loads the node key at `path`, or creates it (parent directories included, file mode
    /// 0600 on Unix) when the file does not exist.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let der = Zeroizing::new(
                fs::read(path)
                    .with_context(|| format!("failed to read node identity {}", path.display()))?,
            );
            return Self::from_pkcs8_der(&der)
                .with_context(|| format!("invalid node identity {}", path.display()));
        }
        let identity = Self::generate()?;
        match write_private_file(path, &identity.pkcs8_der, false) {
            Ok(()) => Ok(identity),
            // Another process created the key first: use the one on disk.
            Err(_) if path.exists() => Self::load_or_create(path),
            Err(e) => {
                Err(e).with_context(|| format!("failed to create node identity {}", path.display()))
            }
        }
    }

    /// The node's 32-byte Ed25519 public key.
    pub fn public_key(&self) -> [u8; NODE_KEY_LEN] {
        self.public_key
    }

    /// The node's public key as 64 lowercase hex characters.
    pub fn public_key_hex(&self) -> String {
        node_key_to_hex(&self.public_key)
    }

    /// A self-signed certificate for [`PERIGEE_SERVER_NAME`] carrying this node's key, plus the
    /// matching private key, ready for rustls.
    pub fn tls_certificate(&self) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let pkcs8 = PrivatePkcs8KeyDer::from(self.pkcs8_der.clone());
        let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &PKCS_ED25519)
            .context("node identity is not a valid Ed25519 PKCS#8 key")?;
        let params = CertificateParams::new(vec![PERIGEE_SERVER_NAME.to_string()])
            .context("failed to build node certificate parameters")?;
        let cert = params
            .self_signed(&key_pair)
            .context("failed to self-sign node certificate")?;
        Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(pkcs8)))
    }
}

impl Zeroize for NodeIdentity {
    fn zeroize(&mut self) {
        self.pkcs8_der.zeroize();
    }
}

impl Drop for NodeIdentity {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("public_key", &self.public_key_hex())
            .field("pkcs8_der", &"<redacted>")
            .finish()
    }
}

/// Formats a node key as 64 lowercase hex characters.
pub fn node_key_to_hex(key: &[u8; NODE_KEY_LEN]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(NODE_KEY_LEN * 2);
    for byte in key {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Parses 64 hex characters (surrounding whitespace ignored) into a node key.
pub fn node_key_from_hex(text: &str) -> Result<[u8; NODE_KEY_LEN]> {
    let text = text.trim();
    if text.len() != NODE_KEY_LEN * 2 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!(
            "a node key is {} hex characters, got '{text}'",
            NODE_KEY_LEN * 2
        );
    }
    let mut key = [0u8; NODE_KEY_LEN];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| anyhow!("a node key contains a non-hex character"))?;
    }
    Ok(key)
}

/// Extracts the Ed25519 node key from a peer certificate. Anything but a well-formed
/// certificate with an Ed25519 `SubjectPublicKeyInfo` is rejected.
pub fn node_key_from_certificate(
    cert: &CertificateDer<'_>,
) -> Result<[u8; NODE_KEY_LEN], rustls::Error> {
    let parsed = webpki::EndEntityCert::try_from(cert)
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let spki = parsed.subject_public_key_info();
    match spki.as_ref().strip_prefix(&ED25519_SPKI_PREFIX[..]) {
        Some(key) if key.len() == NODE_KEY_LEN => {
            let mut out = [0u8; NODE_KEY_LEN];
            out.copy_from_slice(key);
            Ok(out)
        }
        _ => Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        )),
    }
}

/// The Perigee config directory from the process environment (see
/// [`default_config_dir_with`]).
pub fn default_config_dir() -> Option<PathBuf> {
    default_config_dir_with(|k| std::env::var(k).ok())
}

/// The Perigee config directory: `PERIGEE_CONFIG_DIR`, else `%APPDATA%\Perigee` on Windows,
/// else `$XDG_CONFIG_HOME/perigee`, else `$HOME/.config/perigee`. Empty values are ignored.
pub fn default_config_dir_with(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| get(name).filter(|value| !value.trim().is_empty());
    if let Some(dir) = non_empty("PERIGEE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows) {
        return non_empty("APPDATA").map(|dir| PathBuf::from(dir).join("Perigee"));
    }
    if let Some(dir) = non_empty("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("perigee"));
    }
    non_empty("HOME").map(|dir| PathBuf::from(dir).join(".config").join("perigee"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("perigee-identity-{name}-{}", std::process::id()))
    }

    fn env_with(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |k| (k == name).then(|| value.to_string())
    }

    #[test]
    fn test_identity_persists_across_loads() {
        let dir = temp_dir("persist");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested").join(IDENTITY_FILE_NAME);
        let first = NodeIdentity::load_or_create(&path).unwrap();
        let second = NodeIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.public_key(), second.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_certificate_carries_node_key() {
        let identity = NodeIdentity::generate().unwrap();
        let (cert, _key) = identity.tls_certificate().unwrap();
        assert_eq!(
            node_key_from_certificate(&cert).unwrap(),
            identity.public_key()
        );
    }

    #[test]
    fn test_non_ed25519_certificate_is_rejected() {
        let other =
            rcgen::generate_simple_self_signed(vec![PERIGEE_SERVER_NAME.to_string()]).unwrap();
        assert!(node_key_from_certificate(other.cert.der()).is_err());
        assert!(node_key_from_certificate(&CertificateDer::from(vec![0x30, 0x00])).is_err());
    }

    #[test]
    fn test_node_key_hex_round_trip() {
        let key = [0xab_u8; NODE_KEY_LEN];
        let text = node_key_to_hex(&key);
        assert_eq!(text, "ab".repeat(NODE_KEY_LEN));
        assert_eq!(node_key_from_hex(&text).unwrap(), key);
        assert_eq!(node_key_from_hex(&format!("  {text}\n")).unwrap(), key);
        assert!(node_key_from_hex("abc").is_err());
        assert!(node_key_from_hex(&"zz".repeat(NODE_KEY_LEN)).is_err());
        assert!(node_key_from_hex(&format!("+{}", "a".repeat(63))).is_err());
    }

    #[test]
    fn test_default_config_dir_precedence() {
        assert_eq!(
            default_config_dir_with(env_with("PERIGEE_CONFIG_DIR", "/tmp/perigee-test")),
            Some(PathBuf::from("/tmp/perigee-test"))
        );
        assert_eq!(default_config_dir_with(|_| None), None);
        #[cfg(unix)]
        {
            assert_eq!(
                default_config_dir_with(env_with("XDG_CONFIG_HOME", "/tmp/xdg")),
                Some(PathBuf::from("/tmp/xdg/perigee"))
            );
            assert_eq!(
                default_config_dir_with(env_with("HOME", "/home/testuser")),
                Some(PathBuf::from("/home/testuser/.config/perigee"))
            );
        }
    }

    #[test]
    fn test_debug_redacts_private_key() {
        let identity = NodeIdentity::generate().unwrap();
        let text = format!("{identity:?}");
        assert!(text.contains("<redacted>"));
        assert!(text.contains(&identity.public_key_hex()));
    }
}
