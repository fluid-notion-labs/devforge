# Dev Lifecycle: Scenario Orchestrator

## Status

Active research thread. Goal: a Rust-based local dev scenario orchestrator
(working name `devforge`) — user-spawned daemon that owns per-service
lifecycle state machines, serves an MCP socket for agents, and feeds both a
TUI (v1) and a GPUI app. Anchored to the shape of a Svelte web app +
Cloudflare Worker API (see `vendor/pview` for the founding topology:
vite dev server, `wrangler dev`, wrangler-local D1, codegen/wasm jobs,
lazy-search auxiliary).

Progress: **M0** (sqlite store, tokio-rusqlite actor, migrations) and **M1**
(IPC socket — verbs + subscribe stream; TUI over the contract; PTY process
supervision with TERM→KILL group stop; vite/wrangler/cargo pattern sets;
jobs) are committed and integration-tested; a daemon + TUI smoke test in a
temp repo passes end to end. One clarification vs `spec.md`: the socket
speaks the `Verb` JSON contract directly (newline-delimited); MCP tool
framing will be layered over the same verbs, not replace them.

## Documents

- **[spec.md](spec.md)** — the design: scenario TOML, declarative providers
  (npm / wrangler / cargo / exec), engine, sqlite state & history, IPC
  contract, MCP tool surface, TUI + GPUI, phase-2 rust cloudflare mirror,
  milestones.
- **[ui.md](ui.md)** — devforge-tui screen mockups, visual language
  (derived from `vendor/pview`), keybindings, and IPC data flow.
- **[survey.md](survey.md)** — state of play: compose, Procfile family,
  Tilt/DevSpace, task runners, nix family, devcontainers, PM2 — and the
  capability matrix showing the gap nobody owns (per-service state machine,
  build signals, profiles, agent interface).

## Vendored references

- **[vendor/pview](../vendor/pview)** — founding topology reference; source
  of the TUI visual language (see `ui.md`).
- **[vendor/narwhal](../vendor/narwhal)** — our own earlier ratatui log
  watcher; inspiration for the devforge-tui log pane with sections/tabs
  from its burst-sealing heuristic (idle timeout) to be folded in with
  seals also driven by state transitions (e.g. compiling→up). The log-pane
  UI plays out from narwhal's UI; a proper revision/planning pass on the
  TUI logs UI is deferred to a later point.

## Key decisions taken

- One Rust binary spawned by the user in-repo; **MCP over a local socket**
  (stdio-per-agent is out). Agents connect to a running orchestrator.
- Front-ends: headless daemon, TUI, and GPUI — all three are clients of one
  IPC contract (control verbs + state stream). TUI first as the functional
  reference; GPUI after the engine is stable.
- Declarative providers, capped at npm/wrangler/cargo/exec in v1 — the
  everything-else-is-`exec` escape hatch keeps the hydra dead.
- Scenarios = **profiles + lazy services + jobs**, not a binary up/down:
  "servers started" is the default, "search" is situational, typegen /
  wasm / db-reset are jobs with staleness badges.
- Build status via output-pattern heuristics (vite/cargo/wrangler pattern
  sets + empty-output and exit-code failure detection), event hooks deferred
  by design.
- Phase-2 (spec'd, not built): rust cloudflare mirror (D1-on-sqlite + DO
  emulation) to swap out `wrangler dev` where it hurts.
