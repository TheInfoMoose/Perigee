## Problem Description
P4-03. `packaging/linux/perigee-host.service:6-13` has no User=, no ConfigurationDirectory=/PERIGEE_CONFIG_DIR; identity.key and trusted_clients land under root's home or default_config_dir() resolves to None if HOME is unset (whether systemd sets HOME here is unverified). macOS plist (:11-15) sets no PERIGEE_CONFIG_DIR/UserName; perigee.wxs has no ServiceInstall. Operators can't run `trust add` against the service's dir without matching it by hand.
Found during: STEP 4R audit (docs/audit/14-phase4-completeness.md).

## Reproduction Steps
1. Install perigee-host.service. 2. systemctl start perigee-host. 3. Check where identity.key is created (or the start error).

## Fix Log & Agent Notes
