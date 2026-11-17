# Audit reports

Phase 1 of `STEPPED_IMPLEMENTATION_PLAN.md` (local, gitignored): a read-only audit of rFerram against its requirements (R1–R12).

| Report | Area | Step |
| :--- | :--- | :---: |
| `01-baseline.md` | Gate results, inventory, dependency graph | 1A |
| `02-protocol-common.md` | Wire protocol, vault, credential models | 1B |
| `03-net.md` | QUIC transport, TLS, VPN binding, teardown | 1C |
| `04-daemon-auth-lifecycle.md` | Daemon auth and session lifecycle | 1D |
| `05-channels-oob.md` | OOB SSH, clipboard, elevation, passthrough | 1E |
| `06-file-transfer-st.md` | File transfer, ST_Rust_Age drift, link-crate fit | 1F |
| `07-media-backends.md` | Capture, encode, audio, input; display modes | 1G |
| `08-client.md` | Client pipeline, render, e2e test | 1H |
| `09-ui-coupling.md` | UI coupling, vendored libcosmic | 1I |
| `10-repo-packaging.md` | Repo hygiene, packaging, CI, sensitive data | 1J |
| `11-traceability.md` | Requirements traceability matrix | 1K |
| `12-claude-validation.md` | Claude's validation of disputed/suspected items | — |
| `13-evaluation.md` | Evaluation against desired function | — |

Each report is written by Gemini, checked with `scripts/validate-step.sh`, read and spot-checked by Claude, then committed.
