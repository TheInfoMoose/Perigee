//! NIST-compliant credential vault (`NistVault`, `CommonError`) for stored session tokens.

use crate::models::VaultTokenModel;
use simply_transfer_crypto::{CryptoError, SecureStorage};
use thiserror::Error;
use zeroize::Zeroize;

/// Errors produced by `perigee-common` vault and profile operations.
#[derive(Debug, Error)]
pub enum CommonError {
    #[error("Vault storage error: {0}")]
    Vault(#[from] CryptoError),
    #[error("Cryptographic key/signature error: {0}")]
    Crypto(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// NIST-Compliant Credential Vault backed by OS Keyring (`keyring-core`) and
/// `simply-transfer-crypto::SecureStorage` (AES-256-GCM encrypted vault).
pub struct NistVault {
    pub(crate) storage: SecureStorage,
}

impl Default for NistVault {
    fn default() -> Self {
        Self::new("com.theinfomoose.perigee")
    }
}

impl NistVault {
    pub fn new(service_namespace: &str) -> Self {
        Self {
            storage: SecureStorage::new(service_namespace),
        }
    }

    /// Stores a `VaultTokenModel` under `profile_id` in the OS keystore / encrypted vault.
    pub fn store_session_token(
        &self,
        profile_id: &str,
        token: &VaultTokenModel,
    ) -> Result<(), CommonError> {
        let key = format!("token_{profile_id}");
        let json = serde_json::to_string(token)?;
        self.storage.set_secret(&key, json)?;
        Ok(())
    }

    /// Retrieves a `VaultTokenModel` for `profile_id`.
    pub fn get_session_token(&self, profile_id: &str) -> Result<VaultTokenModel, CommonError> {
        let key = format!("token_{profile_id}");
        let mut raw = self.storage.get_secret(&key)?;
        let parsed = serde_json::from_str::<VaultTokenModel>(&raw);
        raw.zeroize();
        Ok(parsed?)
    }

    /// Deletes a stored session token.
    pub fn delete_session_token(&self, profile_id: &str) -> Result<(), CommonError> {
        let key = format!("token_{profile_id}");
        self.storage.delete_secret(&key)?;
        Ok(())
    }
}
