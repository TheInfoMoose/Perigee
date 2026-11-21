//! SSH pairing through `simply-transfer-link`.
//!
//! A host opens a short pairing window with [`PairingWindow`]: an SSH listener that accepts one
//! one-time secret and nothing else. A client that knows the host's node key and the secret runs
//! [`pair_with_host`]; the host adds the client's node key to its `trusted_clients` file and the
//! window closes. Both sides use their node's own Ed25519 identity as the SSH key, so the host
//! key a client pins for mutual TLS is the key that signs the SSH handshake.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::public::Ed25519PublicKey;
use russh::keys::{PrivateKey, PublicKey};
use simply_transfer_link::LinkError;
use simply_transfer_link::client::{HostPin, LinkClient};
use simply_transfer_link::server::LinkServer;
use simply_transfer_link::trust::TrustStore;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::identity::{NODE_KEY_LEN, NodeIdentity};
use crate::trust::TrustedKeys;

/// Default TCP port of a host's pairing window (one above the QUIC port).
pub const DEFAULT_PAIRING_PORT: u16 = 4434;

/// How long a pairing window stays open when nobody pairs.
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(600);

/// The client a pairing window admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedClient {
    pub key: [u8; NODE_KEY_LEN],
    pub label: String,
}

/// This node's identity as an SSH private key (the same Ed25519 key).
pub fn ssh_private_key(identity: &NodeIdentity) -> Result<PrivateKey> {
    let seed = identity.ed25519_seed()?;
    let keypair = Ed25519Keypair::from_seed(&seed);
    if keypair.public.0 != identity.public_key() {
        bail!("node identity seed does not match its public key");
    }
    Ok(PrivateKey::from(keypair))
}

/// A node key as an SSH public key.
pub fn ssh_public_key(node_key: &[u8; NODE_KEY_LEN]) -> PublicKey {
    PublicKey::from(Ed25519PublicKey(*node_key))
}

/// The `simply-transfer-link` host pin (SHA-256 of the SSH public key) for a host node key.
pub fn host_pin(node_key: &[u8; NODE_KEY_LEN]) -> HostPin {
    HostPin::of(&ssh_public_key(node_key))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Pairing-only trust store: accepts one live secret, never admits a key to a full SSH session,
/// and writes the paired client into the host's `trusted_clients` file.
struct PairingTrustStore {
    trusted: TrustedKeys,
    ttl: Duration,
    secret: Mutex<Option<(String, Instant)>>,
    outcome: Mutex<Option<Result<PairedClient, String>>>,
    done: Notify,
}

impl TrustStore for PairingTrustStore {
    fn is_trusted(&self, _key: &PublicKey) -> bool {
        false
    }

    fn add(&self, key: &PublicKey, label: &str) {
        let outcome = match key.key_data().ed25519() {
            Some(ed25519) => self
                .trusted
                .add(ed25519.0, label)
                .map(|()| PairedClient {
                    key: ed25519.0,
                    label: label.to_string(),
                })
                .map_err(|e| format!("{e:#}")),
            None => Err("the pairing client's key is not Ed25519".to_string()),
        };
        *lock(&self.outcome) = Some(outcome);
        self.done.notify_one();
    }

    fn issue_pair_secret(&self) -> String {
        let bytes: [u8; 16] = rand::random();
        let mut secret = String::with_capacity(32);
        for b in bytes {
            let _ = write!(secret, "{b:02x}");
        }
        *lock(&self.secret) = Some((secret.clone(), Instant::now() + self.ttl));
        secret
    }

    fn consume_pair_secret(&self, secret: &str) -> bool {
        let mut slot = lock(&self.secret);
        let valid = matches!(
            slot.as_ref(),
            Some((expected, expiry))
                if Instant::now() < *expiry
                    && constant_time_eq(expected.as_bytes(), secret.as_bytes())
        );
        if valid {
            *slot = None;
        }
        valid
    }
}

/// A host's open pairing window: an SSH listener that admits one client holding the secret.
pub struct PairingWindow {
    listener: TcpListener,
    host_key: PrivateKey,
    store: Arc<PairingTrustStore>,
    secret: String,
    ttl: Duration,
}

impl PairingWindow {
    /// Binds the pairing listener on `addr` and issues a one-time secret valid for `ttl`. A
    /// paired client is written to `trusted_clients_path`.
    pub async fn bind(
        identity: &NodeIdentity,
        trusted_clients_path: PathBuf,
        addr: SocketAddr,
        ttl: Duration,
    ) -> Result<Self> {
        let host_key = ssh_private_key(identity)?;
        let trusted = TrustedKeys::load(trusted_clients_path)?;
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind pairing listener on {addr}"))?;
        let store = Arc::new(PairingTrustStore {
            trusted,
            ttl,
            secret: Mutex::new(None),
            outcome: Mutex::new(None),
            done: Notify::new(),
        });
        let secret = store.issue_pair_secret();
        Ok(Self {
            listener,
            host_key,
            store,
            secret,
            ttl,
        })
    }

    /// The one-time secret the client must present.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The bound TCP address (useful when binding port 0 in tests).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// Serves the window until one client pairs (`Ok(Some(..))`), the secret expires
    /// (`Ok(None)`), or the listener fails. The listener closes when this returns.
    pub async fn run(self) -> Result<Option<PairedClient>> {
        let store = self.store.clone();
        let server = LinkServer::builder()
            .host_key(self.host_key)
            .trust(store.clone())
            .serve_on(self.listener);
        tokio::select! {
            result = server => {
                result.map_err(|e| anyhow!("pairing listener failed: {e}"))?;
                bail!("pairing listener stopped unexpectedly")
            }
            () = store.done.notified() => match lock(&store.outcome).take() {
                Some(Ok(client)) => Ok(Some(client)),
                Some(Err(message)) => bail!("pairing failed: {message}"),
                None => bail!("pairing finished without an outcome"),
            },
            () = tokio::time::sleep(self.ttl) => Ok(None),
        }
    }
}

/// Pairs this node with the host's pairing window at `addr`: checks the host proves it holds
/// `host_key`, presents `secret`, and asks the host to trust this node under `label`.
pub async fn pair_with_host(
    identity: &NodeIdentity,
    addr: SocketAddr,
    host_key: &[u8; NODE_KEY_LEN],
    secret: &str,
    label: &str,
) -> Result<()> {
    let key = ssh_private_key(identity)?;
    match LinkClient::pair(addr, host_pin(host_key), secret.trim(), &key, label).await {
        Ok(()) => Ok(()),
        Err(LinkError::HostKeyMismatch) => bail!(
            "the host at {addr} did not prove it holds the expected host key; check the key `perigee-host identity` prints"
        ),
        Err(e) => Err(anyhow!("pairing with {addr} failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TRUSTED_CLIENTS_FILE_NAME;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("perigee-pairing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_ssh_key_is_the_node_key() {
        let identity = NodeIdentity::generate().unwrap();
        let key = ssh_private_key(&identity).unwrap();
        assert_eq!(
            key.public_key().key_data(),
            ssh_public_key(&identity.public_key()).key_data()
        );
    }

    #[tokio::test]
    async fn test_pairing_adds_client_to_trusted_clients() {
        let dir = temp_dir("ok");
        let host = NodeIdentity::generate().unwrap();
        let client = NodeIdentity::generate().unwrap();
        let path = dir.join(TRUSTED_CLIENTS_FILE_NAME);
        let window = PairingWindow::bind(
            &host,
            path.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        let addr = window.local_addr().unwrap();
        let secret = window.secret().to_string();
        let host_key = host.public_key();
        let (paired, client_result) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                window.run(),
                pair_with_host(&client, addr, &host_key, &secret, "testuser-laptop"),
            )
        })
        .await
        .expect("pairing timed out");
        client_result.unwrap();
        assert_eq!(
            paired.unwrap(),
            Some(PairedClient {
                key: client.public_key(),
                label: "testuser-laptop".to_string(),
            })
        );
        assert!(
            TrustedKeys::load(path)
                .unwrap()
                .contains(&client.public_key())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_pairing_rejects_wrong_secret_and_wrong_host_key() {
        let dir = temp_dir("reject");
        let host = NodeIdentity::generate().unwrap();
        let client = NodeIdentity::generate().unwrap();
        let impostor = NodeIdentity::generate().unwrap();
        let path = dir.join(TRUSTED_CLIENTS_FILE_NAME);
        let window = PairingWindow::bind(
            &host,
            path.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let addr = window.local_addr().unwrap();
        let secret = window.secret().to_string();
        let host_key = host.public_key();
        let impostor_key = impostor.public_key();
        let (outcome, wrong_secret, wrong_host) = tokio::join!(
            window.run(),
            pair_with_host(
                &client,
                addr,
                &host_key,
                "00000000000000000000000000000000",
                "testuser-a",
            ),
            pair_with_host(&client, addr, &impostor_key, &secret, "testuser-b"),
        );
        assert!(wrong_secret.is_err());
        let err = wrong_host.unwrap_err();
        assert!(format!("{err:#}").contains("did not prove"), "got: {err:#}");
        assert_eq!(outcome.unwrap(), None);
        assert!(!path.exists() || TrustedKeys::load(path).unwrap().entries().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
