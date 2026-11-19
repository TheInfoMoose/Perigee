**Repo:** Perigee, a Rust remote-desktop workspace (QUIC transport, wgpu, libcosmic UI). Crates: `perigee-common`, `perigee-protocol`, `perigee-net`, `perigee-channels`, `perigee-host`, `perigee-client`, `perigee-ui`, plus vendored `vendor/ST_Rust_Age/*` and `vendor/libcosmic`.

**Source of truth:** the step's Argon block in `/workspace/STEPPED_IMPLEMENTATION_PLAN.md` and its "Who runs what" header. **Read `/workspace/AGENTS.md` before editing**: it's binding (error boundaries, zeroize, `// SAFETY:` comments, postcard wire format, no dependency drift).

**Domain skills.** Read the matching one before touching a crate:
- `/workspace/.agents/skills/perigee-wire-and-quic/SKILL.md`: `perigee-common`, `perigee-protocol`, `perigee-net`
- `/workspace/.agents/skills/perigee-st-channels/SKILL.md`: `perigee-channels`
- `/workspace/.agents/skills/perigee-capture-and-compositor/SKILL.md`: `perigee-host`, `perigee-client`
- `/workspace/.agents/skills/perigee-cosmic-iced-ui/SKILL.md`: `perigee-ui`
- `/workspace/.agents/skills/perigee-quality-gate/SKILL.md`: what the host gates check

**Rules:**
- Gates (run on the host by the operator): `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. Every cargo call takes `--locked`.
- Vendored crates stay vendored. Never edit `vendor/` unless the step lists it.
- Never edit anything outside this repo (the upstream ST_Rust_Age repo included), and never edit the plan file.
- Test data uses placeholders only (`testuser`, `example.com`, `192.0.2.0/24`).
