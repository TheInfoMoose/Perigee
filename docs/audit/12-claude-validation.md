# 12 Claude validation
Audited HEAD: f3189ba

Step 2 of the audit workflow. Claude validated each Gemini report (01–11) as it landed, then ran three read-only subagents on the items parked during validation:
1. Auth and crypto chain, plus severity calls (COM-01, NET-03, AUTH-01..03, NET-01 + COM-02, CH-01).
2. Every quoted value behind the 57 Critical/High findings in reports 01–10.
3. Transport and dependency details (BASE-05, NET-04, AUTH-05, LIFE-01).

Claude re-checked the subagents' key claims against the code before recording them here. Each item is **Confirmed**, **Refuted** or **Reclassified**, with evidence. Confidence labels follow the Audit Report Contract (`Read`, `Ran`, `Suspected`).

## Summary
- **The evidence base holds.** All 57 Critical/High findings were re-checked quote by quote: 56 OK, 1 with wrong line numbers (BASE-05). Gemini invented specifics only where already corrected (06 commit mapping, 10 quoted literals).
- **Six severities change.** Two Criticals drop to High (COM-01, NET-03). Four Highs drop (BASE-05 → Low; CH-01, AUTH-05 → Medium; NET-04 → High on Windows/BSD/macOS but Low on Linux for `SO_REUSEPORT`).
- **The real exposure is simpler than the finding count suggests.** Any remote peer gets a full session (AUTH-01), and the client trusts any host (NET-01). Everything else in R3 sits behind those two.
- **Adjusted totals:** 5 Critical, 49 High, 33 Medium, 15 Low (102 findings), down from 7 / 50 / 31 / 14.

## Severity changes
| Finding | Was | Now | Why | Evidence | Confidence |
| :--- | :--- | :--- | :--- | :--- | :--- |
| COM-01 | Critical | **High** | Needs local access. Aggravating factor: the `/tmp` fallback can be planted (below). | `vendor/ST_Rust_Age/simply-transfer-crypto/src/lib.rs:89-90, 96, 115-118` | Read |
| NET-03 | Critical | **High** | Remote unauthenticated DoS: one bad handshake stops the daemon. The contract's Critical tier is access, execution or exposure, not availability. | `crates/rferram-net/src/endpoint.rs:188-193`; `crates/rferram-server/src/main.rs:40, 63` | Read |
| BASE-05 | High | **Low** | Neither vulnerable API is reachable. The real impact is that the CI `security-audit` job fails. | Below | Ran |
| CH-01 | High | **Medium** | The elevation flag is set by any peer, but nothing outside `#[cfg(test)]` reads it. Stubbed feature, no escalation. | `crates/rferram-channels/src/elevation.rs:120`; readers only in test modules (`daemon.rs:1067` under `:835`, `session_controller.rs:1106` under `:821`, `channels/src/lib.rs:191-270` under `:72`) | Read |
| AUTH-05 | High | **Medium** | Unauthenticated memory/connection exhaustion (DoS), not exposure. Wording fixed below. | `crates/rferram-server/src/daemon.rs:546-549`; `main.rs:40-59`; `daemon.rs:467-480` | Read |
| NET-04 | High | **High (Windows, BSD/macOS); Low (Linux `SO_REUSEPORT`)** | A local user can take over port 4433. Because the client verifies no server cert (NET-01), that user can then impersonate the host to clients. | Below | Read + Suspected (OS semantics) |

### COM-01 detail
- **Key:** `SHA-256("SimplyTransfer:NIST-SP-800-57:VaultKey:" + service + $USER)` (`lib.rs:115-118`). On Linux the keyring is never used (`NoDefaultStore`, `:44`), so this key is the only protection.
- **Normal path:** `$HOME/.simply-transfer` gets `0o700` (`:96`); files are written, then set to `0o600` (`:169, :173`). Other users are blocked by the directory.
- **`/tmp` fallback:** used when `$HOME/.simply-transfer` can't be created (`:86-91`). Both the `create_dir_all` and the `0o700` chmod results are discarded (`let _ =`). If another local user creates `/tmp/simply-transfer-vault` first, the chmod fails silently and that user can read and replace every vault file written there. With the computable key, that means plaintext secrets. (Read)

### BASE-05 detail
- **Real lockfile lines:** russh 0.64.1 at `Cargo.lock:3194`; rustix 1.1.5 at `Cargo.lock:3330`. The report's `:291/:298` point at `aws-lc` packages. (Read)
- **RUSTSEC-2026-0495 (russh):** `russh::keys::agent::server` signs with confirm-constrained keys without asking. Patched in `>= 0.65.0`. No `keys::agent` use anywhere in `crates/` or `vendor/ST_Rust_Age`. (Ran: `cargo deny check advisories`, grep)
- **RUSTSEC-2026-0486 (rustix):** informational "unsound" in `getxattr`/`listxattr` with zero-length buffers. Patched in `>= 1.1.6`. Pulled in by gethostname, polling, wayland-* and x11rb; none call xattr functions. (Ran: `cargo tree --locked -i`, grep)
- **Correction to an assumption:** russh is not only in `oob_ssh.rs`. The vendored `simply-transfer-core/src/russh_{client,server}.rs` also use it; neither touches the agent module.

### NET-04 detail
OS semantics below are from documentation recalled by the subagent, not tested here (Suspected).
- **Linux `SO_REUSEPORT`:** sockets sharing a port must have the same effective UID. Only a same-user process can join, and it could already kill the daemon. Low.
- **Linux `SO_REUSEADDR` on UDP:** may allow a different user to bind a more specific address and receive unicast datagrams. **Unverified; needs a two-user test.**
- **macOS/BSD:** with both flags set, a duplicate bind succeeds with no UID check.
- **Windows:** setting `SO_REUSEADDR` without `SO_EXCLUSIVEADDRUSE` opts into cross-account port stealing (`socket.rs:89`; `set_reuse_port` is skipped on Windows at `:90-91`).

### AUTH-05 detail
- **Wording fix:** quinn 0.11.12 `ReadToEndBuffer` starts as `Vec::new()` and allocates the final buffer only when the stream finishes (`quinn-0.11.12/src/recv_stream.rs:456-458, 479`). There's no preallocation, so the accurate claim is "buffers up to 16 MiB of attacker-sent data per connection". (Read)
- **Stands:** there's no handshake timeout before the first `recv_uni_message` (`daemon.rs:546-549`). The 10 s idle timeout (`rferram-net/src/lib.rs:34`) is defeated by keep-alives. There's also no cap on connections or sessions.

## Confirmed as rated
| Finding | Severity | Validation | Evidence |
| :--- | :--- | :--- | :--- |
| AUTH-01 | Critical | Confirmed. A `ClientHello` with non-empty fields returns a session ID; `serve_connection` sends success and provisions. | `daemon.rs:379-399, 586-601` |
| AUTH-02 | Critical | Confirmed. **Independent bypass**, not redundant: if AUTH-01 were fixed alone, any self-generated Ed25519 keypair still passes (no allowlist). | `daemon.rs:412-426` |
| AUTH-03 | Critical | Confirmed. **Independent bypass:** `SystemUserPassword` accepts any single byte. Initial `VaultToken` *does* compare against the stored token (`:444`), so that path is not a bypass. In-session re-auth gates nothing: `valid` only feeds the reply (`:738-748`). | `daemon.rs:444, 454-457, 735-748` |
| NET-01 | Critical | Confirmed. See the chain below. | `endpoint.rs:222-255` |
| LIFE-01 | High | Confirmed. `run_capture_loop` and `start_loopback_capture` can only return `Ok`, so the one non-test `terminate_session` call is dead code. | `daemon.rs:130-208, 604-606`; `audio.rs:64-94` |

**For planning:** treat AUTH-01..03 as **one "no authentication" blocker with three sub-items**. A fix must close all three paths (ClientHello grant, unallowlisted Ed25519, password stub), plus the in-session re-auth that gates nothing.

## NET-01 + COM-02 chain
| Question | Answer | Why |
| :--- | :--- | :--- |
| Can an active MITM impersonate a host to the client? | **Yes** | `verify_server_cert` always passes and CertificateVerify is never checked (`endpoint.rs:222-255`). Even a pinned fingerprint wouldn't prove key possession. |
| Can a captured `AuthRequest` replay against the same daemon? | **Yes, until restart** | Client-chosen challenge (`daemon.rs:419`), no server nonce or replay cache. The cert hash only has to match the daemon's own (`:565-576`), which stays fixed until the next bind (`endpoint.rs:120`). |
| Against a different host? | **No** | Different cert, different hash, rejected at `daemon.rs:571`. Old captures also die at restart. |
| Practical exposure today | **MITM of the client + anyone in at the server** | The live client only sends `ClientHello` (`client/src/session.rs:150-162`), so channel binding protects nothing in the shipped flow. And with no key allowlist, an attacker doesn't need to replay; they sign with their own key. |

## Evidence re-check
Every Critical/High finding in reports 01–10 (57) was checked against the cited `file:line`, by grepping each quoted token, string, constant and function name.

| Result | Count | Findings |
| :--- | :---: | :--- |
| OK | 56 | All others |
| DRIFT | 1 | BASE-05 (wrong `Cargo.lock` lines; corrected above) |
| WRONG | 0 | — |

- Quotes in UI-01 and UI-03 are condensed (multi-line code on one line; UI-01 omits a `record_connecting_status` call). Every token is real and at the cited lines.
- Not re-counted: PROTO-01's variant numbers (8 dead, 12 lacking sender or handler).
- Earlier corrections stand: `06-file-transfer-st.md` and `10-repo-packaging.md` each end with a "Claude validation correction" section.

## Other parked items resolved
- **`render.rs` (01 vs 08):** Stub (CLI-03). Settled by 1H.
- **Client never sends `AuthRequest`:** Confirmed (1D, and again here).
- **COM-02 scope:** the server *does* compare the cert hash. Replay is limited to the same daemon run; see the chain above. Claude's earlier note saying otherwise was wrong.
- **Audio contradiction (11, item 2):** the server *does* send zero-filled audio frames and the client buffers them. `10-repo-packaging.md`'s README table is wrong on that point; AUD-01 and CLI-07 are right.
- **Duplicates, counted once for planning:** CLI-08 = PROTO-03; CLI-10 ⊂ AUTH-04 + PROTO-04 + CH-03; BASE-01 = ENC-01; BASE-02 = ST-02; BASE-03 = NET-10 = ST-05; COM-05, CH-04, UI-09 ⊂ SENS-01/02.
- **OOB-02..05:** latent while the OOB server never starts (OOB-01). One planning item: replace `oob_ssh.rs` with `simply-transfer-link`.

## Adjusted severity totals
| | Critical | High | Medium | Low | Total |
| :--- | :---: | :---: | :---: | :---: | :---: |
| Reports 01–10 as written | 7 | 50 | 31 | 14 | 102 |
| After this validation | **5** | **49** | **33** | **15** | 102 |

The 5 Criticals left: AUTH-01, AUTH-02, AUTH-03 (one blocker), NET-01, COM-02.

## Still unverified
- **Linux `SO_REUSEADDR` cross-user UDP bind (NET-04).** Needs a two-user test on a real host. It doesn't change the plan: removing both flags is the fix either way.
- **Upstream ST_Rust_Age defects** (identical snapshot drivers, `check_server_key` returning `Ok(true)`, `mock_hash`). Verified to exist at `efeaa92`. Fixing them belongs to the ST plan, not rFerram's.

## Inputs for Step 3 (evaluation)
- Go/no-go, and keep / refactor / rebuild per crate, with effort (user request, 2026-11-17).
- R10: upstream libcosmic, or rename the in-repo toolkit (UI-04).
- Headless vs screen share first (R6); audio in the first milestone or deferred; elevation and passthrough deferred (CH-01, CH-02).
- Whether rFerram waits for ST Phase 4 (`simply-transfer-link`) or co-develops it.
- VPN ingestion in or out (R1); scope of the lightweight destination app (R9).
- BASE-07: dlopen vs linking `libxkbcommon`.
