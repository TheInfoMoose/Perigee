## Problem Description
`crates/perigee-common/src/vault.rs` — after Phase 4 STEP 4Q deletes the Ed25519 channel-binding helpers, the NistVault session-token store (store/get/delete_session_token, VaultTokenModel) has no callers in the workspace — suggested fix: in Phase 4b, either reuse it for simply-transfer-link session tickets or delete it.
Found during: Phase 4 planning.

## Reproduction Steps
n/a (planned work). After STEP 4Q: `grep -rn 'session_token' crates --include=*.rs` shows only vault.rs and its tests.

## Fix Log & Agent Notes
