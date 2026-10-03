# AGENTS.md — devforge

Local dev scenario orchestrator (Rust). See `plan/index.md` (status),
`plan/spec.md` (design), `docs/architecture.md` (crate layout).

## Layout

- `crates/devforge-core` — engine library: scenario TOML (`config`), service
  state machine (`state`), IPC contract types (`ipc`), providers
  (`provider`, foo/npm/wrangler/cargo/exec), errors (`error`, thiserror +
  anyhow). No UI code here.
- `crates/devforge-tui` — ratatui front-end. A pure IPC client of
  `devforge-core::ipc` verbs + stream. Nothing imports engine internals.
- `crates/devforge-gpui` — M4, scaffold only, **excluded from the build**
  (see its README).

## Rules

- `plan/*.md` is the source of design truth; keep specs in sync when
  deviating.
- Error style: `thiserror` enums per crate (`EngineError`, `TuiError`),
  `anyhow` only at the binary boundary.
- Engine owns all state (sqlite, later); clients never touch the file.
- Provider cap: npm / wrangler / cargo / exec. Everything else is `exec`.
- Do not add dependencies without noting why (see `docs/architecture.md`).
- gpui crate stays out of the build until M4.

## Committing

- One commit per task or milestone step (e.g. "TUI scaffold", "M0 store" /
  "socket listener + verb dispatch"). Stages grow once split.
- Commit as soon as all checks are green (build / clippy / fmt / tests),
  without asking; commit users' signed-off milestones with a summary line
  against `plan/spec.md` milestone numbers (e.g. `M0:` prefix).
- Do not bundle unrelated changes into one commit; `plan/docs` sync counts as
  its own commit when the design deviates.

## Build / check

```
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```
