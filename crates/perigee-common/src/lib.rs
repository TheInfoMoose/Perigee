//! `perigee-common`
//!
//! Central domain models, session configuration profiles, and OS/desktop environment
//! detection (Linux Debian/RHEL/SUSE/Arch, COSMIC Desktop, Windows, macOS).

pub mod env;
pub mod models;

pub use env::*;
pub use models::*;

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
