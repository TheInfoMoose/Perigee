# Perigee Workspace Agent Rules (`AGENTS.md`)

These rules apply unconditionally to all AI agents operating within the **Perigee** workspace.

## 1. Execution & Context Discipline
- **Single-Task Atomic Execution**: Execute one task from `STEPPED_IMPLEMENTATION_PLAN.md` (and see `docs/audit/` for audit reports) at a time. Never bundle unrelated crate changes into a single turn.
- **Module Extraction & Re-Export Invariant**: When extracting submodules (e.g., `src/models.rs`, `src/vault.rs`, `src/socket.rs`, `src/endpoint.rs`) from an existing crate's `src/lib.rs`, **always** `pub use` the public types, traits, and functions at the root of `src/lib.rs` so downstream workspace crates compile without breaking imports.
- **Activate Domain Skills First**: Before modifying any crate, read the corresponding workspace skill in `.agents/skills/`:
  - `perigee-quality-gate`: Mandatory pre-completion verification sequence.
  - `perigee-wire-and-quic`: For `perigee-common`, `perigee-protocol`, and `perigee-net`.
  - `perigee-st-channels`: For `perigee-channels` (`arboard` clipboard; legacy OOB SSH, snapshot transfer, elevation, and passthrough removed in Phase 2; replaced by `simply-transfer-link`).
  - `perigee-capture-and-compositor`: For `perigee-host` and `perigee-client` (`wgpu`, `rav1e`, Wayland/Smithay, DXGI, ScreenCaptureKit).
  - `perigee-cosmic-iced-ui`: For `perigee-ui` (`libcosmic`, `cosmic-config`, Elm pattern, WGSL shaders).

## 2. Strict Toolchain & Dependency Controls
- **Toolchain Baseline**: Rust `1.93+`, `edition = "2024"`, Cargo workspace `resolver = "3"`.
- **Locked Builds Only**: Every `cargo check`, `cargo clippy`, `cargo test`, and `cargo build` invocation **MUST** pass `--locked`.
- **Zero Dependency Drift**: Do **NOT** add, remove, or modify crate versions in `Cargo.toml` or `Cargo.lock` unless explicitly instructed by the user.
- **Vendored Crates**: `ST_Rust_Age` (`simply-transfer-link`; `simply-transfer-crypto` and `simply-transfer-snapshots` stay on disk but are not in the crate graph) and `libcosmic` (`libcosmic`, `cosmic-config`) are vendored in `vendor/` and patched via `[patch]`. Never replace them with remote git/crates.io dependencies without explicit user instruction.
- **Wire Serialization Standard**: Always use `postcard` (`perigee_protocol::encode_wire_message`, `decode_wire_message`, `encode_length_prefixed`, `decode_length_prefixed`) with `MAX_WIRE_FRAME_BYTES` (16 MiB) bounds checking. Do **NOT** introduce `bincode`.

## 3. Code Quality, Safety & Error Handling
- **Error Boundaries**:
  - Library crates (`perigee-common`, `perigee-protocol`) **MUST** define explicit typed errors via `thiserror::Error`.
  - Transport, channel, daemon, and UI crates (`perigee-net`, `perigee-channels`, `perigee-client`, `perigee-host`, `perigee-ui`) may use `anyhow::Result` with `.context(...)` at subsystem boundaries alongside typed domain errors.
- **Memory & Cryptographic Hygiene**:
  - All secret and token structs (`NodeIdentity` in `perigee-net`) **MUST** implement `zeroize::Zeroize` and zeroize on `Drop`, and redact secrets in `fmt::Debug`.
  - Node identity is the only long-lived secret: `NodeIdentity` (Ed25519, `<config dir>/identity.key`, 0600, written via `fsutil::write_private_file`). Trust lives in `trusted_clients` / `known_hosts` (`TrustedKeys`). Never log a pairing secret.
  - Never allow unannotated `unsafe` blocks. Every FFI call (`libc::setsockopt`, Win32 `SendInput`/`DXGI`, macOS `objc2`/`CGEvent`, Wayland/DRM/X11 handles) **MUST** be preceded by a `// SAFETY:` comment explaining pointer validity, lifetime, and thread-safety invariants.
- **Documentation & Linting**: Preserve existing module docstrings (`//!`) and item docstrings (`///`). All code must pass `cargo clippy --workspace --all-targets --locked -- -D warnings` with zero warnings.

## 4. Mandatory Quality Gate
Before declaring any task complete, you **MUST** run the verification script from `.agents/skills/perigee-quality-gate/scripts/verify-crate.sh <target-crate>` (or the equivalent `--locked` commands):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p <target-crate> --all-targets --locked
```
