//! Read-only Linux kernel/DRM/`vkms` probe for headless operation.
//!
//! Inspects sysfs and configfs paths without requiring root privileges or mutating
//! kernel state, helping operators verify whether a connected DRM output (physical
//! or `vkms` virtual connector) is available for `cosmic-comp` KMS.

use std::fmt::Write;
use std::fs;
use std::path::Path;

/// Status of a single DRM connector discovered under `/sys/class/drm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmConnectorStatus {
    /// Connector entry name (e.g., `card0-DP-1`, `card1-Virtual-1`).
    pub name: String,
    /// Trimmed contents of `<connector>/status` (e.g., `connected`, `disconnected`, `unknown`).
    pub status: String,
    /// Whether the connector name indicates a virtual connector (`Virtual`, case-insensitive).
    pub is_virtual: bool,
}

/// Summary of Linux headless/DRM/`vkms` readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessProbeReport {
    /// Kernel release string (from `/proc/sys/kernel/osrelease` or `"unknown"`).
    pub kernel_release: String,
    /// Whether `/sys/module/vkms` exists (i.e., the `vkms` module is loaded).
    pub vkms_loaded: bool,
    /// Whether `/sys/kernel/config/vkms` exists (configfs dynamic connector interface).
    pub vkms_configfs_available: bool,
    /// Discovered DRM connectors sorted by name.
    pub connectors: Vec<DrmConnectorStatus>,
}

/// Probes arbitrary filesystem paths simulating `/sys/module/vkms`,
/// `/sys/kernel/config/vkms`, and `/sys/class/drm`.
pub fn probe_headless_paths(
    sys_module_vkms: &Path,
    configfs_vkms: &Path,
    sys_class_drm: &Path,
    kernel_release: String,
) -> HeadlessProbeReport {
    let vkms_loaded = sys_module_vkms.exists();
    let vkms_configfs_available = configfs_vkms.exists();

    let mut connectors = Vec::new();
    if let Ok(entries) = fs::read_dir(sys_class_drm) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card") || !name.contains('-') {
                continue;
            }
            let status_path = entry.path().join("status");
            let status = fs::read_to_string(&status_path)
                .ok()
                .map(|raw| raw.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            let is_virtual = name.to_ascii_lowercase().contains("virtual");
            connectors.push(DrmConnectorStatus {
                name,
                status,
                is_virtual,
            });
        }
    }
    connectors.sort_by(|a, b| a.name.cmp(&b.name));

    HeadlessProbeReport {
        kernel_release,
        vkms_loaded,
        vkms_configfs_available,
        connectors,
    }
}

/// Probes the live Linux system paths (`/sys/module/vkms`, `/sys/kernel/config/vkms`,
/// `/sys/class/drm`, and `/proc/sys/kernel/osrelease`) without modifying kernel state.
pub fn probe_headless_system() -> HeadlessProbeReport {
    let kernel_release = fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    probe_headless_paths(
        Path::new("/sys/module/vkms"),
        Path::new("/sys/kernel/config/vkms"),
        Path::new("/sys/class/drm"),
        kernel_release,
    )
}

/// Formats a [`HeadlessProbeReport`] as a human-readable multi-line status report.
pub fn format_headless_report(report: &HeadlessProbeReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Kernel release: {}", report.kernel_release);
    let _ = writeln!(
        out,
        "vkms module:    {}",
        if report.vkms_loaded {
            "loaded"
        } else {
            "not loaded"
        }
    );
    let _ = writeln!(
        out,
        "vkms configfs:  {}",
        if report.vkms_configfs_available {
            "available"
        } else {
            "not available"
        }
    );
    let _ = writeln!(out, "DRM connectors:");
    if report.connectors.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for conn in &report.connectors {
            let kind = if conn.is_virtual {
                "virtual"
            } else {
                "physical"
            };
            let _ = writeln!(out, "  {} [{}]: {}", conn.name, kind, conn.status);
        }
    }

    let connected_count = report
        .connectors
        .iter()
        .filter(|c| c.status == "connected")
        .count();
    if connected_count > 0 {
        let _ = write!(
            out,
            "Status: ready ({connected_count} connected DRM connector(s))"
        );
    } else {
        let _ = write!(
            out,
            "Status: not ready (0 connected DRM connectors). \
             Run `sudo modprobe vkms` (or configure dynamic connectors via \
             /sys/kernel/config/vkms on Linux >= 6.12), or attach a hardware EDID dummy plug."
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_probe_headless_paths_detects_vkms_and_connectors() {
        let root = TempDirGuard::new("perigee-headless-probe");
        let sys_module_vkms = root.path().join("sys_module_vkms");
        let configfs_vkms = root.path().join("configfs_vkms");
        let sys_class_drm = root.path().join("sys_class_drm");

        fs::create_dir_all(&sys_module_vkms).unwrap();
        fs::create_dir_all(&configfs_vkms).unwrap();
        fs::create_dir_all(sys_class_drm.join("card0-DP-1")).unwrap();
        fs::create_dir_all(sys_class_drm.join("card1-Virtual-1")).unwrap();
        // Non-connector entries should be ignored.
        fs::create_dir_all(sys_class_drm.join("card0")).unwrap();
        fs::create_dir_all(sys_class_drm.join("renderD128")).unwrap();

        fs::write(sys_class_drm.join("card0-DP-1/status"), "disconnected\n").unwrap();
        fs::write(sys_class_drm.join("card1-Virtual-1/status"), "connected\n").unwrap();

        let report = probe_headless_paths(
            &sys_module_vkms,
            &configfs_vkms,
            &sys_class_drm,
            "6.12.0-test".to_string(),
        );

        assert_eq!(report.kernel_release, "6.12.0-test");
        assert!(report.vkms_loaded);
        assert!(report.vkms_configfs_available);
        assert_eq!(
            report.connectors,
            vec![
                DrmConnectorStatus {
                    name: "card0-DP-1".to_string(),
                    status: "disconnected".to_string(),
                    is_virtual: false,
                },
                DrmConnectorStatus {
                    name: "card1-Virtual-1".to_string(),
                    status: "connected".to_string(),
                    is_virtual: true,
                },
            ]
        );

        let formatted = format_headless_report(&report);
        assert!(formatted.contains("6.12.0-test"));
        assert!(formatted.contains("card0-DP-1"));
        assert!(formatted.contains("disconnected"));
        assert!(formatted.contains("card1-Virtual-1"));
        assert!(formatted.contains("connected"));
        assert!(formatted.contains("ready"));
    }

    #[test]
    fn test_format_headless_report_recommends_vkms_when_disconnected() {
        let report = HeadlessProbeReport {
            kernel_release: "6.11.0-test".to_string(),
            vkms_loaded: false,
            vkms_configfs_available: false,
            connectors: vec![DrmConnectorStatus {
                name: "card0-HDMI-A-1".to_string(),
                status: "disconnected".to_string(),
                is_virtual: false,
            }],
        };

        let formatted = format_headless_report(&report);
        assert!(formatted.contains("not loaded"));
        assert!(formatted.contains("not ready"));
        assert!(formatted.contains("modprobe vkms"));
        assert!(formatted.contains("dummy plug"));
    }
}
