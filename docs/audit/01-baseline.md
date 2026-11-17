# 01 Baseline audit
Audited HEAD: d62b895

## Summary
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0 on `rustc 1.99.1`, while `cargo test --workspace --all-targets --locked` and the `rferram-server` smoke test exit 101 on this host because `rferram-server` links `-lxkbcommon` (`libxkbcommon.so` from `libxkbcommon-dev` is absent, only `libxkbcommon.so.0` is present); the other 6 workspace crates pass all 80 executed tests.
- `cargo deny check advisories bans licenses sources` exits 1 due to two RustSec advisories (`RUSTSEC-2026-0495` in `russh 0.64.1` and `RUSTSEC-2026-0486` in `rustix 1.1.5`) plus 37 duplicate crate warnings across the lockfile.
- Across `[workspace.dependencies]`, 28 of 68 declared entries (including `rav1e`, `smithay-client-toolkit`, `drm`, `gbm`, `input`, `libseat`, `udev`, `calloop`) are not referenced by any `crates/*/Cargo.toml`, and 11 crate-level dependencies are never used in their crate's `src/`.
- `simply-transfer-core` is declared in `crates/rferram-channels/Cargo.toml` (`L-BASE-2`) and `rav1e` in root `Cargo.toml` (`L-BASE-1`), but neither is used in source; video encoding and ST core link transport are stubbed or bypassed.
- The CI smoke test (`RFERRAM_SMOKE_TEST=1`) returns `Ok(())` immediately after `QuicServerListener::bind` without accepting or exercising a session (`L-BASE-4`).

## Gate results

### Toolchain versions (`2f`)
- `rustc --version` (exit `0`): `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- `cargo --version` (exit `0`): `cargo 1.99.1 (5f94df478 2026-08-27)`

### `2a. cargo fmt --all -- --check`
- Exit code: `0`

### `2b. cargo clippy --workspace --all-targets --locked -- -D warnings`
- Exit code: `0`

### `2c. cargo test --workspace --all-targets --locked`
- Exit code: `101` (`rferram-server` failed to link `-lxkbcommon`; per-crate test runs for the remaining 6 crates all exited `0`)
- Per-crate test counts (`cargo test -p <crate> --all-targets --locked`):
  - `rferram-common`: 3 passed, 0 failed, 0 ignored (unit)
  - `rferram-protocol`: 13 passed, 0 failed, 0 ignored (3 unit + 10 in `tests/wire_tests.rs`)
  - `rferram-net`: 12 passed, 0 failed, 0 ignored (7 unit + 5 in `tests/quic_tests.rs`)
  - `rferram-channels`: 11 passed, 0 failed, 0 ignored (unit)
  - `rferram-client`: 15 passed, 0 failed, 0 ignored (14 unit + 1 in `tests/e2e_loopback.rs`)
  - `rferram-ui`: 26 passed, 0 failed, 0 ignored (25 lib unit + 1 bin unit)
  - `rferram-server`: link failure (exit `101`); 21 `#[test]` / `#[tokio::test]` functions exist in `crates/rferram-server/src/` (Confidence: Read)
- Last 20 lines of `cargo test --workspace --all-targets --locked`:
```text
   Compiling rferram-ui v0.1.0 (/srv/devshare/rFerram/crates/rferram-ui)
error: linking with `cc` failed: exit status: 1
  |
  = note:  "cc" "-m64" "/srv/devshare/rFerram/target/debug/deps/rustcC3vjaB/symbols.o" "<4 object files omitted>" "-Wl,--as-needed" "-Wl,-Bstatic" "<rlibs omitted>" "-Wl,-Bdynamic" "-lxkbcommon" "-ldl" "-lgcc_s" "-lutil" "-lrt" "-lpthread" "-lm" "-ldl" "-lc" "-L" "/srv/devshare/rFerram/target/debug/deps/rustcPm4Tv9/raw-dylibs" "-B<sysroot>/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld" "-fuse-ld=lld" "-Wl,--eh-frame-hdr" "-Wl,-z,noexecstack" "-L" "/srv/devshare/rFerram/target/debug/build/aws-lc-sys-f814431429e516bc/out" "-L" "/srv/devshare/rFerram/target/debug/build/ring-4df27c3db158f686/out" "-L" "<sysroot>/lib/rustlib/x86_64-unknown-linux-gnu/lib" "-o" "/srv/devshare/rFerram/target/debug/deps/rferram_server-b0ff96a218493e94" "-Wl,--gc-sections" "-pie" "-Wl,-z,relro,-z,now" "-nodefaultlibs"
  = note: some arguments are omitted. use `--verbose` to show all linker arguments
  = note: rust-lld: error: unable to find library -lxkbcommon
          collect2: error: ld returned 1 exit status
          

error: could not compile `rferram-server` (lib test) due to 1 previous error
```

### `2d. RFERRAM_SMOKE_TEST=1 cargo run -p rferram-server --locked`
- Exit code: `101`
- Last 20 lines:
```text
   Compiling wayland-backend v0.3.17
   Compiling x11rb-protocol v0.13.2
   Compiling wgpu-types v30.0.1
   Compiling image v0.25.10
   Compiling wgpu-naga-bridge v30.0.1
   Compiling wgpu-hal v30.0.1
   Compiling gethostname v1.1.0
   Compiling wayland-client v0.31.15
   Compiling wgpu-core-deps-windows-linux-android v30.0.1
   Compiling wgpu-core v30.0.1
   Compiling x11rb v0.13.2
   Compiling wayland-protocols v0.32.13
   Compiling arboard v0.3.7
   Compiling rferram-channels v0.1.0 (/srv/devshare/rFerram/crates/rferram-channels)
   Compiling wayland-protocols-wlr v0.3.12
   Compiling wgpu v30.0.1
   Compiling rferram-server v0.1.0 (/srv/devshare/rFerram/crates/rferram-server)
error: linking with `cc` failed: exit status: 1
  |
  = note: rust-lld: error: unable to find library -lxkbcommon
          collect2: error: ld returned 1 exit status

error: could not compile `rferram-server` (bin "rferram-server") due to 1 previous error
```

### `2e. cargo deny check advisories bans licenses sources`
- Exit code: `1` (`advisories FAILED, bans ok, licenses ok, sources ok` — failed on `RUSTSEC-2026-0495` in `russh 0.64.1` and `RUSTSEC-2026-0486` in `rustix 1.1.5`)
- Last 20 lines:
```text
      │   │   │   ├── wayland-protocols-plasma v0.3.12
      │   │   │   │   └── winit v0.30.13 (*)
      │   │   │   ├── wayland-protocols-wlr v0.3.12
      │   │   │   │   ├── rferram-server v0.1.0 (*)
      │   │   │   │   └── smithay-client-toolkit v0.19.2 (*)
      │   │   │   └── winit v0.30.13 (*)
      │   │   ├── wayland-protocols-plasma v0.3.12 (*)
      │   │   ├── wayland-protocols-wlr v0.3.12 (*)
      │   │   └── winit v0.30.13 (*)
      │   ├── wayland-csd-frame v0.3.0
      │   │   └── smithay-client-toolkit v0.19.2 (*)
      │   ├── wayland-protocols v0.32.13 (*)
      │   ├── wayland-protocols-plasma v0.3.12 (*)
      │   ├── wayland-protocols-wlr v0.3.12 (*)
      │   └── winit v0.30.13 (*)
      ├── wayland-client v0.31.15 (*)
      ├── wayland-cursor v0.31.14 (*)
      └── x11rb v0.13.2 (*)

advisories FAILED, bans ok, licenses ok, sources ok
```

## Status matrix
| Capability | Status (Real/Partial/Stub/Missing) | Evidence | Req |
| :--- | :--- | :--- | :--- |
| `rferram-common` (363 non-test src LOC; 3 tests: 3 passed, 0 failed, 0 ignored; internal deps: none [vendored: `simply-transfer-crypto`]) | Real | `crates/rferram-common/src/vault.rs:64-129`, `models.rs:41-122`, `env.rs:36-88` | R3, R11, R12 |
| `rferram-protocol` (432 non-test src LOC; 13 tests: 13 passed, 0 failed, 0 ignored; internal deps: none) | Real | `crates/rferram-protocol/src/lib.rs:72-463`, `tests/wire_tests.rs:1-461` | R3, R4, R5, R11 |
| `rferram-net` (621 non-test src LOC; 12 tests: 12 passed, 0 failed, 0 ignored; internal deps: `rferram-common`, `rferram-protocol`) | Partial | Real `quinn`/`socket2` in `crates/rferram-net/src/endpoint.rs:116-206`, `socket.rs:77-138`; stubbed TLS verifier in `endpoint.rs:222-255` | R1, R3, R7, R11 |
| `rferram-channels` (1057 non-test src LOC; 11 tests: 11 passed, 0 failed, 0 ignored; internal deps: `rferram-common`, `rferram-protocol` [vendored: `simply-transfer-core`, `simply-transfer-crypto`, `simply-transfer-snapshots`]) | Partial | Real `russh` + snapshots in `crates/rferram-channels/src/oob_ssh.rs:163-358`, `snapshot_transfer.rs:60-156`; simulated elevation/passthrough in `elevation.rs:88-140`, `passthrough.rs:86-152` | R3, R4, R5, R8, R11 |
| `rferram-client` (1519 non-test src LOC; 15 tests: 15 passed, 0 failed, 0 ignored; internal deps: `rferram-channels`, `rferram-common`, `rferram-net`, `rferram-protocol`) | Partial | Real QUIC bridge and `wgpu` pipeline in `crates/rferram-client/src/bridge.rs:64-240`, `render.rs:265-510`; synthetic decoder/audio in `session.rs:163-216`, `audio.rs:114-184` | R2, R6, R7, R12 |
| `rferram-server` (4809 non-test src LOC; 21 tests in src [link failed on `-lxkbcommon`]; internal deps: `rferram-channels`, `rferram-common`, `rferram-net`, `rferram-protocol`) | Partial | Real QUIC daemon loop in `crates/rferram-server/src/daemon.rs:365-620`; stubbed encoder slicing raw bytes in `encoder.rs:83-84, 153-154` | R2, R3, R6, R7, R9, R12 |
| `rferram-ui` (2512 non-test src LOC; 26 tests: 26 passed, 0 failed, 0 ignored; internal deps: `rferram-channels`, `rferram-client`, `rferram-common`, `rferram-net`, `rferram-protocol` [vendored: `cosmic-config`, `libcosmic`]) | Partial | Real `libcosmic` UI shell in `crates/rferram-ui/src/app.rs:580-1560`; coupled with simulated fallback in `session_controller.rs:260-360` | R2, R9, R10, R12 |

### Internal dependency graph
- `rferram-common` -> *(none; vendored: `simply-transfer-crypto`)*
- `rferram-protocol` -> *(none)*
- `rferram-net` -> `rferram-common`, `rferram-protocol`
- `rferram-channels` -> `rferram-common`, `rferram-protocol` *(vendored: `simply-transfer-core`, `simply-transfer-crypto`, `simply-transfer-snapshots`)*
- `rferram-client` -> `rferram-channels`, `rferram-common`, `rferram-net`, `rferram-protocol`
- `rferram-server` -> `rferram-channels`, `rferram-common`, `rferram-net`, `rferram-protocol`
- `rferram-ui` -> `rferram-channels`, `rferram-client`, `rferram-common`, `rferram-net`, `rferram-protocol` *(vendored: `cosmic-config`, `libcosmic`)*
- **Cycle / app-crate dependency check:** No dependency cycles exist, and no library crate depends on an app crate (`rferram-server` or `rferram-ui`), although `rferram-server` and `rferram-ui` are unnecessarily listed in root `[workspace.dependencies]` (`Cargo.toml:32-33`).

### Unused dependencies (Confidence: Read)
- **Declared in `crates/*/Cargo.toml` `[dependencies]` (or target `[dependencies]`) but never used in that crate's `src/`:**
  - `crates/rferram-common/Cargo.toml`: `tracing` (`:20`)
  - `crates/rferram-protocol/Cargo.toml`: *(none)*
  - `crates/rferram-net/Cargo.toml`: `libc` (`:14`), `quinn-proto` (`:16`), `rferram-common` (`:18`, used only in `tests/quic_tests.rs`)
  - `crates/rferram-channels/Cargo.toml`: `sha2` (`:17`), `simply-transfer-core` (`:18`)
  - `crates/rferram-client/Cargo.toml`: `bytemuck` (`:13`), `rferram-channels` (`:14`), `rferram-common` (`:15`, used only in `tests/e2e_loopback.rs`)
  - `crates/rferram-server/Cargo.toml`: `wgpu` (`:25`), `wayland-backend` (`:29`), `windows-sys` (`:38`)
  - `crates/rferram-ui/Cargo.toml`: *(none — `libcosmic` exports crate name `cosmic`, which is used in `src/`)*
- **Declared in root `Cargo.toml` `[workspace.dependencies]` but not referenced by any `crates/*/Cargo.toml` (28 crates):**
  - `aes-gcm` (`:106`), `async-trait` (`:121`), `base64` (`:108`), `calloop` (`:62`), `drm` (`:63`), `futures` (`:120`), `gbm` (`:65`), `glow` (`:44`), `image` (`:50`), `input` (`:66`), `keyring-core` (`:114`), `libseat` (`:67`), `notify` (`:53`), `notify-rust` (`:54`), `png` (`:51`), `rav1e` (`:94`), `raw-window-handle` (`:46`), `rfd` (`:52`), `rferram-server` (`:32`), `rferram-ui` (`:33`), `smithay-client-toolkit` (`:57`), `sysinfo` (`:115`), `taffy` (`:49`), `tempfile` (`:129`), `tiny-skia` (`:48`), `udev` (`:68`), `walkdir` (`:128`), `winit` (`:45`).

## Leads
| Lead | Verdict | Evidence | Finding |
| :--- | :--- | :--- | :--- |
| L-BASE-1: `rav1e` is in `[workspace.dependencies]` but not in `crates/rferram-server/Cargo.toml`. | Confirmed | `Cargo.toml:94`, `crates/rferram-server/Cargo.toml:15-47`, `crates/rferram-server/src/encoder.rs:153-154` | BASE-01 |
| L-BASE-2: `simply-transfer-core` is declared in `crates/rferram-channels/Cargo.toml` but never used. | Confirmed | `crates/rferram-channels/Cargo.toml:18`, zero occurrences of `simply_transfer_core` in `crates/rferram-channels/src/` | BASE-02 |
| L-BASE-3: `rustls` enables both `ring` and `aws-lc-rs`; `quinn` uses `rustls-ring`; `russh` uses `aws-lc-rs` (two crypto providers). | Confirmed | `Cargo.toml:97-103` | BASE-03 |
| L-BASE-4: the CI smoke test (step 2d) only proves the server binds; it exercises no session. | Confirmed | `crates/rferram-server/src/main.rs:29-38`, `.github/workflows/ci.yml:128-132` | BASE-04 |

## Findings

### BASE-01: `rav1e` is declared only in root workspace dependencies and never wired into `rferram-server`
- Severity: High
- Category: stub
- Requirement: R6
- Evidence: `Cargo.toml:94` and `crates/rferram-server/src/encoder.rs:153-154`
```rust
        let payload_size = std::cmp::min(1024, frame.data.len());
        let encoded_payload = frame.data[..payload_size].to_vec();
```
- Confidence: Read
- Impact: `rferram-server` never performs AV1 video encoding; `SoftwareVideoEncoder` truncates raw pixel buffers to 1024 bytes.
- Direction: Wire a real encoder implementation into `rferram-server` and remove or connect unused workspace media dependencies.

### BASE-02: `simply-transfer-core` is declared in `rferram-channels` but completely unused
- Severity: High
- Category: structure
- Requirement: R11
- Evidence: `crates/rferram-channels/Cargo.toml:18-20`
```toml
simply-transfer-core = { workspace = true }
simply-transfer-crypto = { workspace = true }
simply-transfer-snapshots = { workspace = true }
```
- Confidence: Read
- Impact: `rferram-channels` compiles `simply-transfer-core` (and its transitive dependencies) without reusing its transport or channel primitives.
- Direction: Replace bespoke duplicated transport/channel glue with upstream `simply-transfer-link` / `simply-transfer-core` or drop unused crate dependencies.

### BASE-03: Dual TLS/SSH cryptographic providers (`ring` and `aws-lc-rs`) linked simultaneously
- Severity: Low
- Category: hygiene
- Requirement: R3
- Evidence: `Cargo.toml:97-103`
```toml
quinn = { version = "0.11.12", default-features = false, features = ["rustls-ring", "runtime-tokio"] }
quinn-proto = "0.11.19"
rustls = { version = "0.23.46", default-features = false, features = ["ring", "aws-lc-rs", "std", "tls12"] }
```
- Confidence: Read
- Impact: Both `ring` and `aws-lc-rs` C/assembly cryptographic backends are compiled and linked into binaries, increasing build time, binary size, and attack surface, and requiring explicit provider selection if `rustls` defaults are invoked.
- Direction: Standardize `rustls`, `quinn`, and `russh` onto a single cryptographic provider across the workspace.

### BASE-04: CI daemon smoke test exits immediately after socket bind without exercising a session
- Severity: Medium
- Category: gap
- Requirement: R2
- Evidence: `crates/rferram-server/src/main.rs:36-38`
```rust
    if std::env::var("RFERRAM_SMOKE_TEST").is_ok() {
        return Ok(());
    }
```
- Confidence: Read
- Impact: `.github/workflows/ci.yml:128-132` passes even if handshake, authentication, stream routing, or session teardown is completely broken.
- Direction: Extend the smoke test or CLI integration test to establish an authenticated loopback session and verify clean teardown.

### BASE-05: `cargo deny` fails on two RustSec advisories (`russh 0.64.1` and `rustix 1.1.5`)
- Severity: High
- Category: security
- Requirement: R3
- Evidence: `Cargo.lock:291` (`russh 0.64.1`, `RUSTSEC-2026-0495`) and `Cargo.lock:298` (`rustix 1.1.5`, `RUSTSEC-2026-0486`)
```text
error[vulnerability]: Agent server signs with confirm-constrained keys without calling `Agent::confirm`
error[unsound]: xattr functions mark unwritten bytes as initialized when given a zero-length buffer
```
- Confidence: Ran (`cargo deny check advisories bans licenses sources`)
- Impact: The CI `security-audit` job fails (`exit 1`) and the lockfile pins vulnerable/unsound versions of `russh` and `rustix`.
- Direction: Update `russh` to `>=0.65.0` and `rustix` to `>=1.1.6` in `Cargo.toml` / `Cargo.lock` during the dependency remediation phase.

### BASE-06: 28 unused `[workspace.dependencies]` entries and 11 unused crate-level dependencies bloat the workspace manifest
- Severity: Low
- Category: hygiene
- Requirement: R11
- Evidence: `Cargo.toml:57-68`
```toml
smithay-client-toolkit = "0.19.2"
wayland-client = "0.31.15"
wayland-backend = "0.3.17"
```
- Confidence: Read
- Impact: Root `Cargo.toml` advertises 28 dependencies (`drm`, `gbm`, `input`, `libseat`, `udev`, `smithay-client-toolkit`, `rav1e`, etc.) that no workspace member uses, while `rferram-net`, `rferram-client`, `rferram-channels`, `rferram-common`, and `rferram-server` pull in 11 unused crate dependencies.
- Direction: Prune unused dependencies from `[workspace.dependencies]` and `crates/*/Cargo.toml`, and move test-only `rferram-common` imports in `rferram-net` and `rferram-client` to `[dev-dependencies]`.

### BASE-07: `rferram-server` links `-lxkbcommon` directly via the `xkbcommon` crate instead of `dlopen` or optional feature gating
- Severity: Medium
- Category: structure
- Requirement: R12
- Evidence: `crates/rferram-server/Cargo.toml:34` and `crates/rferram-server/src/backends/linux_input.rs:215`
```rust
        let sym = xkbcommon::xkb::Keysym::new(key_code);
```
- Confidence: Ran (`cargo test --workspace --all-targets --locked`)
- Impact: Building or testing `rferram-server` on Linux fails at link time (`unable to find library -lxkbcommon`) whenever `libxkbcommon-dev` (`libxkbcommon.so` development symlink) is not installed, even though `libxkbcommon.so.0` is present.
- Direction: Either gate native backend linkage or use `xkbcommon-dl` / pure keysym tables so headless builds and tests do not fail when `-dev` symlink packages are absent.

## Open questions for Claude
- Should `rferram-server`'s dependency on `xkbcommon` be replaced with `xkbcommon-dl` (already in `Cargo.lock` via `smithay-client-toolkit`) during the implementation phase so `cargo test --workspace` links without requiring `libxkbcommon-dev` on the host?
