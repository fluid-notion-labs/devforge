# Architecture

Three-crate workspace, one IPC contract. `plan/spec.md` is the design source;
this file records the concrete crate layout and dependency decisions.

```
              ┌──────────────────────┐
              │   devforge-core      │  engine library
              │  config / state /    │  (scenario TOML, state machine,
              │  ipc / provider /    │   IPC types, providers, errors)
              │  store(sqlite, M0)   │
              └───────┬──────────────┘
        Verb + StreamEvent over local socket (JSON)
        ┌─────────────┴─────────────┐
┌───────┴──────────┐      ┌─────────┴─────────┐
│  devforge-tui    │      │   devforge-gpui   │
│  ratatui, M1     │      │   M4, not built   │
│  (in workspace)  │      │  (excluded)       │
└──────────────────┘      └───────────────────┘
```

## Crates

- **`devforge-core`** — the engine as a library; the `devforge` daemon binary
  will be a thin `main` over it. Contains and owns everything stateful:
  scenario parsing (`config`), the service state machine (`state`),
  the IPC contract (`ipc`: `Verb` request/responses + `StreamEvent` pushes),
  providers (`provider`: npm/wrangler/cargo/exec, hydra-capped), sqlite store
  (M0). No UI types.
- **`devforge-tui`** — ratatui client of the IPC contract. Pure client:
  connects to the socket, sends verbs, consumes the stream, redraws.
  Proves the contract: if the TUI needs an engine-internal, the contract is
  wrong — fix the contract, not the TUI.
- **`devforge-gpui`** — scaffold only, excluded so the build stays clean until
  M4. Same client position as the TUI.

## Error strategy

- `thiserror` enums per crate: `devforge_core::error::EngineError`,
  `devforge_tui::TuiError`. Structured, matchable, `Send`.
- `anyhow` only at the binary boundary (`main` returns `anyhow::Result`).
- Cores never import each other's errors.

## Dependencies (and why)

Workspace-pinned in the root `Cargo.toml`; add nothing per-crate without a
one-liner here.

| crate | why |
|---|---|
| `thiserror` | structured error enums per crate |
| `anyhow` | top-level `main` boundary only |
| `tokio` | single engine runtime; sockets, process spawn |
| `serde` / `serde_json` | the IPC contract is JSON (verbs + stream) |
| `toml` | scenario file parsing |
| `indexmap` | preserve scenario file order of services/profiles in UI listings |
| `shell-words` | split `exec`/`cargo` command strings into argv |
| `portable-pty` | service children run under a PTY so ANSI output survives (spec: Engine) |
| `libc` | `kill` on the child's process group for TERM/KILL stop semantics |
| `tracing` / `tracing-subscriber` | engine logging (to file, not the TUI); `env-filter` feature for `RUST_LOG` control |
| `ratatui`, `crossterm` | TUI front-end |
| `rusqlite` (`bundled`), `tokio-rusqlite`, `rusqlite_migration` | M0 store: embedded sqlite, no system dep; single connection behind a tokio actor; `user_version` migrations |
| `gpui` (workspace, unused) | reserved for M4; uncommented only when the gpui crate activates |

Deferred, planned: `rusqlite` (M0 store), `portable-pty` (PTY spawn, spec
§Engine), an MCP SDK or hand-rolled socket framing (MCP is JSON over the
socket; evaluate when the verbs stabilize).

## Invariants

1. Exactly one engine API: `devforge_core::ipc`. TUI/GPUI/MCP are three
   clients; anything a UI can do, an agent can do.
2. Seat of truth is the engine (sqlite); clients never open the DB file.
3. Provider cap: npm/wrangler/cargo/exec; all else is `exec` + patterns.
4. The gpui crate stays out of the build until M4.
