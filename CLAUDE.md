@AGENTS.md

## Claude Code notes
- `AGENTS.md` above is authoritative. Read the matching skill in `.agents/skills/<name>/SKILL.md` before touching a crate; run `.agents/skills/perigee-quality-gate/scripts/verify-crate.sh <crate>` before finishing.
- Always pass `--locked` to cargo. Don't change `Cargo.toml`/`Cargo.lock` versions unless asked. `vendor/` crates stay vendored.
