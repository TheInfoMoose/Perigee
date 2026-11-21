//! `perigee-common`
//!
//! Central domain models, session configuration profiles, OS/desktop environment
//! detection (Linux Debian/RHEL/SUSE/Arch, COSMIC Desktop, Windows, macOS), and
//! NIST-compliant credential vault abstractions backed by `keyring-core` and `simply-transfer-crypto`.

pub mod env;
pub mod models;
pub mod vault;

pub use env::*;
pub use models::*;
pub use vault::*;

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroize;

    #[test]
    fn test_vault_session_token_roundtrip() {
        let vault = NistVault::new("test.perigee.common");

        let token = VaultTokenModel {
            session_token: "nist-test-session-token".to_string(),
            auth_method: "Ed25519+P2P".to_string(),
            expires_at: 1_950_000_000,
        };
        vault
            .store_session_token("unit_test_profile", &token)
            .expect("Failed to store session token");
        let loaded_token = vault
            .get_session_token("unit_test_profile")
            .expect("Failed to retrieve session token");
        assert_eq!(token, loaded_token);
        vault
            .delete_session_token("unit_test_profile")
            .expect("Failed to delete session token");
        assert!(vault.get_session_token("unit_test_profile").is_err());
    }

    #[test]
    fn test_runtime_environment_detection() {
        let env = RuntimeEnvironment::detect();
        if cfg!(target_os = "linux") {
            assert!(matches!(
                env.platform,
                PlatformDistribution::LinuxDebian
                    | PlatformDistribution::LinuxRhel
                    | PlatformDistribution::LinuxSuse
                    | PlatformDistribution::LinuxArch
                    | PlatformDistribution::LinuxGeneric
            ));
        }

        assert_eq!(
            RuntimeEnvironment::parse_os_release("ID=cachyos\nID_LIKE=arch"),
            PlatformDistribution::LinuxArch
        );
        assert_eq!(
            RuntimeEnvironment::parse_os_release("ID=pop\nID_LIKE=\"ubuntu debian\""),
            PlatformDistribution::LinuxDebian
        );
        assert_eq!(
            RuntimeEnvironment::parse_os_release("ID=fedora"),
            PlatformDistribution::LinuxRhel
        );
        assert_eq!(
            RuntimeEnvironment::parse_os_release("ID=\"opensuse-tumbleweed\""),
            PlatformDistribution::LinuxSuse
        );
    }

    #[test]
    fn test_profile_serialization_and_credential_redaction() {
        let profile = ConnectionProfile {
            friendly_name: "Dev Workstation".to_string(),
            group_name: "Engineering".to_string(),
            vpn_binding: Some("wg0".to_string()),
            folder_shares: vec!["/tmp/perigee-test".to_string()],
            ..Default::default()
        };

        assert_eq!(profile.name(), "Dev Workstation");
        assert_eq!(profile.group(), "Engineering");

        let json = serde_json::to_string(&profile).expect("Failed to serialize ConnectionProfile");
        let decoded: ConnectionProfile =
            serde_json::from_str(&json).expect("Failed to deserialize ConnectionProfile");
        assert_eq!(profile, decoded);

        let mut token = VaultTokenModel {
            session_token: "testuser-token-fixture".to_string(),
            auth_method: "Ed25519+P2P".to_string(),
            expires_at: 1_900_000_000,
        };
        let token_debug = format!("{token:?}");
        assert!(token_debug.contains("[REDACTED]"));
        assert!(!token_debug.contains("testuser-token-fixture"));
        token.zeroize();
        assert!(token.session_token.is_empty());
        assert_eq!(token.expires_at, 0);
    }
}
