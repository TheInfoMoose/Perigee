## Problem Description
Remaining macOS/Windows part of BUG-0004 (Linux fixed in STEP 5O, 8f771c4). The macOS launchd plist sets no PERIGEE_CONFIG_DIR or UserName, and perigee.wxs has no ServiceInstall, so operators cannot run trust add against the service dir. Suggested fix: per-platform service account plus an explicit config dir, as packaging/linux/perigee-host.service now does.
Found during: Phase 5a review.

## Reproduction Steps
n/a (packaging review)

## Fix Log & Agent Notes
