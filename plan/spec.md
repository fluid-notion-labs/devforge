# devforge: a Rust Local Dev Scenario Orchestrator

## Status

Design spec (no code yet). Founding use case: a Svelte web app talking to a
Cloudflare Worker API, mirroring the topology of `vendor/pview` (vite dev server
+ `wrangler dev` + wrangler-local D1 + codegen/wasm build steps + optional
auxiliary services). The tool is general-purpose but anchored to that shape.

Core decisions already taken:

- One Rust binary, spawned **by the user** in their repo. It owns the engine,
  persists state in sqlite, and serves MCP over a local socket so agents
  connect to it (stdio-per-agent is out).
- Front-ends: headless daemon, plus **both** a TUI (v1, functional reference)
  and a GPUI app (target GUI). Both are clients of the same IPC contract.
- Declarative providers, not a plugin runtime. v1 provider set is capped at
  `npm`, `wrangler`, `cargo`, `exec` — everything else is `exec`. No hydra.

## Motivation

Managing a local dev stack today, as pview does it:

- `npm run dev:all` = `concurrently -k "npm run dev" "npm --prefix ../client/web run dev"` —
  union lifecycle only; one crash takes everything down via `-k`; no
  per-service restart; no notion of which service is *ready*.
- `scripts/proc.sh` = shell pidfile helper: start/stop/status/log, survives
  the terminal (setsid detach). It solved detached start/stop the dumb way —
  which is fine — but owns no state beyond a pid, no readiness, no history,
  no build signals.
- `scripts/dev-setup.mjs` = idempotent preflight (npm install, wasm target,
  wasm-pack, ts-rs typegen into the client, D1 migrate + seed, `.dev.vars`),
  staleness tracked with stamp files. Excellent content, invisible interface:
  you must *know* it exists and re-run it manually when core sources change.
- Dev DB reset (`npm run reset`) is a separate manual command.

What's missing everywhere in this glue: a **named abstraction of "the scenario
I'm working in"**, a **lifecycle state machine per service**, **build status
signals**, **full history of what ran**, and an interface agents can use
without shelling out through npm-script archaeology.

Typical session intents:

- "servers started" — the common case: web + api, both up, both watched.
- "just the frontend" — scratch on UI alone.
- "local dev search" — the (xiu-style) ingest server is only wanted while
  actively working on ingest; starting it alongside everything else wastes
  boot time, port space and attention. So **lazy services**: not started with
  the default profile, startable on demand, stoppable when done.
- "the stack changed under me" — I touched `pview-core`, so typegen + wasm
  + D1 may be stale. That's a **job**, not a long-running service.

## Scenario file

`dev-scenario.toml` at repo root (name TBD; `devforge` reads first match:
project root, then `.devforge/scenario.toml`).

```toml
[scenario]
name = "pview"

[services.web]
provider = "npm"
cwd = "client/web"
script = "dev"                      # from package.json — enumerated in the UI
ready_when = "vite"                 # provider-known readiness pattern
port = 5173

[services.api]
provider = "wrangler"
cwd = "deploy"
command = "dev"                     # wrangler dev --ip 0.0.0.0 (from package.json: "dev")
port = 8787
env_file = ".dev.vars"              # provided by preflight job

[services.search]                   # lazy on purpose
provider = "exec"
cwd = "deploy"
command = "sh ./xiu/run.sh"
lazy = true                         # excluded from profile "all"; manual start

[services.markserv]
provider = "exec"
command = "npx markserv plans/master.md"
lazy = true

[jobs.setup]                        # preflight; run when stale or forced
cwd = "."
command = "node scripts/dev-setup.mjs"

[jobs.db_reset]
cwd = "deploy"
command = "npm run reset"

[profiles]
all      = ["web", "api"]           # the usual state
scratch  = ["web"]                   # UI only
ingest   = ["web", "api", "search"]  # includes the lazy one explicitly

[mcp]
enabled = true
socket = ".devforge/socket"         # unix socket / localhost port; printed on boot

[watchdog]
lazy_idle_stop = "20m"              # optional v2: auto-demote idle lazy services
```

Design points:

- **`npm` provider enumerates `package.json` scripts.** This is the TS/JS
  entry: rather than re-encoding commands, the orchestrator reads scripts and
  offers them (in TUI/CLI) as bindable service commands. `wrangler dev` means
  `npm run dev` inside `deploy/` — same mechanism, provider adds wrangler
  -specific knowledge (local D1 paths, `--ip` flag, migrate/seed subcommands,
  `.dev.vars` handling).
- **Profiles are named startup sets.** Default profile = `all`. Starting a
  profile starts its non-`lazy` services and any explicitly listed lazy ones.
- **Soft / situational deps.** Not a static DAG. `after = [...]` is advisory:
  when profile `all` starts, `api` first, then `web` (vite proxies /api and
  /ws to :8787). When profile `scratch` starts, web alone. Dependency
  evaluation is **per-profile**, not transitive-transitive-all: the
  orchestrator does not auto-start `api` just because `web` usually follows
  it — if a profile starts `web` without its usual companion, that is a
  warning toast, not a surprise boot of the worker. (Failure mode avoided:
  "I typed start and woke up all 9 processes.")
- **Jobs** are run-to-completion tasks with status output and a tail; they
  can also be marked `stale_when` (e.g. `setup` when `rust/crates/core/src/**`
  mtimes newer than the stamp) — v1 policy: surface staleness as a badge and
  offer the job, do not auto-run.

## Providers

Declarative, not a plugin ABI. Each provider is a small built-in module that
knows (a) how to spawn, (b) its readiness/compile/failure patterns, (c) any
idioms worth knowing (npm scripts enumeration; wrangler's local D1 watching,
port drift; cargo's pkg selection).

| Provider | Spawns | Knows |
|---|---|---|
| `npm` | `npm run <script>` in cwd | package.json script enumeration (bindable from file or UI); vite detection |
| `wrangler` | `wrangler dev …` | local mode; D1 migrate/seed/reset verbs; `Ready on http://…` |
| `cargo` | `cargo run/…` | workspace pkgs; crate selection; watch mode optional |
| `exec` | raw argv | nothing; readiness only via port probe or explicit pattern |

The hydra cap: a fifth provider is cheap to *spec* but we don't. Anything
not in the v1 set is `exec` + explicit patterns. If a provider earns its
place by being used twice via `exec` with identical pattern blocks, it gets
promoted — by evidence, not enthusiasm.

## Engine

- Single tokio runtime. Services spawn with a **PTY** (via `portable-pty`) so
  color/ANSI output survives (vite and cargo both emit ANSI — without a PTY
  the TUI gets raw escape soup or stripped text; with one we keep signals and
  colors).
- Stop semantics: send SIGTERM to the **process group** (setsid child like
  `proc.sh` does), wait for graceful exit up to N seconds, then SIGKILL the
  group. This fixes concurrent's "dies on one crash" and shell script "child
  survives TERM" classes of bug.
- Optional detached mode: `devforge serve` keeps running after the terminal
  closes (same setsid trick), so an agent-connected MCP session keeps working
  while the user's TUI is closed. State lives in sqlite, not in daemon
  memory, so any number of clients observe consistently.

### Service state machine

```
idle -> starting -> up <-> compiling -> failed
   ^                 |                    |
   +----- stopping <-+--------------------+
idle:        known, not running; lazy services live here by default
starting:    process spawned, no readiness signal yet
up:          readiness signal observed (pattern match or port open)
compiling:   per-provider build signals while service is up
             (vite HMR, cargo recompile, wrangler reload)
failed:      exit != 0 or characteristic crash pattern or empty output
stopping:    TERM sent, awaiting exit (bounded)
```

Transitions are recorded to sqlite with timestamp and cause (user request,
provider pattern, watchdog, crash). This is the shared vocabulary across
TUI, GPUI, and MCP.

## State & history (sqlite)

`.devforge/state.sqlite3`. Single source of truth; daemon and clients all
read/write through the engine (clients go over IPC, never open the file
themselves).

- `runs` — one row per service start attempt: service, profile, pid, started_at,
  ended_at, exit code, cwd, command.
- `events` — appended rows: timestamp, service, kind (state_transition, log_line,
  build_signal, mcp_call, job_result), payload. Log lines go here at line
  granularity; build patterns become structured `build_signal` rows.
- `state` — current engine view: service → state, port, last exit, attached
  profile.

This is what makes "the build output is recorded for full history and I can
query it" cheap: log tails in the UI are `SELECT` + reattach stream, agent
queries go through `events`, and nothing needs to be re-piped.

## IPC contract (load-bearing section)

The daemon exposes two channels over the same socket:

1. **Control verbs** (JSON, request/response):
   `scenario_start(profile)`, `scenario_stop`, `service_start(name, wait?)`,
   `service_restart(name)`, `service_stop(name)`, `job_run(name)`,
   `scenario_status()`, `service_logs(name, tail)`, `event_query(filter)`,
   `scenario_reload()`.
2. **State stream** (JSON, subscribe/push): every transition and
   build_signal emitted as it happens, so front-ends redraw from a stream
   rather than polling.

Seat of truth is the daemon. TUI, GPUI, and MCP chat about the same verbs.
This symmetry is deliberate: **there is exactly one engine API**, and MCP /
TUI / GPUI are just three clients of it. Anything the UI can do, an agent
can do, and vice versa.

## MCP server

Transport: the orchestrator listens on a local socket (`~/.devforge/socket`
or per-repo `.devforge/socket` per `[mcp] socket`). Agents connect as MCP
clients of that socket; possibly many at once (the daemon fans state to all).

Discovery: spawned orchestrator writes the socket path into
`.devforge/socket` (gitignored) and prints it; agents in the repo pick it up
by well-known path rather than config archaeology.

Tools (agents get the state machine, not raw pids):

- `scenario_status` — services with state/port/last log line; current
  profile; jobs and staleness flags.
- `scenario_start(profile?)` — start a profile. Idempotent: starting the
  running profile = no-op (returns current state), or restart-if-flagged.
- `service_start(name, wait_for_ready?)` — blocks until `up` or `failed`
  with a timeout. This is the agent win: no fire-and-hope.
- `service_restart(name)` / `service_stop(name)` / `scenario_stop`
- `job_run(name)` — runs a job (codegen, db_reset, …) and returns completion
  status + tail.
- `service_logs(name, tail)` / `event_query(filter)` — reading the sqlite
  history (build outcomes, restart counts, etc.).

Authorization posture: since the user spawns the daemon, MCP clients are
trusted to the same degree as local processes. Optional `mcp.read_only` flag
in the scenario file (v2 concern, noted) gives agents observe-only sessions
if wanted — not a v1 promise.

## Build status

Two layers, deliberately ordered:

1. **Signals from output patterns**, per provider — pragmatic, portable:

| Provider | up / ready | compiling | failure |
|---|---|---|---|
| vite | `ready in <n> ms` | `hmr update …` | non-zero exit; tracebacks |
| wrangler | `Ready on http://…` | `watching for file changes…` reload | compile errors; exit != 0 |
| cargo | server pattern from run output (or explicit pattern block in scenario file) | `Compiling <crate>` / `Finished` | `error[E…]` / `error:` |
| exec | user pattern or port probe | — | exit != 0 |

   These map to the state machine's `up` / `compiling` / `failed`
   transitions and are recorded as `build_signal` events. Per-service UI
   dot: grey = idle, yellow = starting/compiling, green = up, red = failed,
   dim = stopping.
   The "does the output do *nothing*" case is covered two ways: (a) a
   **cancel-watch** window (if a service said `Compiling` but `Finished`
   never arrives, or `ready` never arrives within N seconds, `starting`
   times-out into `failed` with the tail attached); (b) zero-byte output +
   dead pid → failed with the empty-mark noted. Not a rabbit hole: gone as
   soon as the service knows its readiness pattern; just two knobs.

2. **Cancel-watch / provider event hooks** (designed, not built): a real
   event bus would let cargo/vite report build completion directly. Spec'd
   as an extension point on the provider trait; heuristics remain the v1
   answer and the hydra-proof default.

## Front-ends

### v1 TUI (functional reference)

ratatui. Left: service list — state dot, name, port, profile membership,
last line of output. Right: pane following the selected service's log (from
sqlite or live stream via IPC). Keystrokes: `s` start, `x` stop, `r`
restart, `L` tails, `j/J` jobs, `p/P` profile switch, `q` quit. A thin bar
at the bottom shows current profile and preflight staleness badges.

The TUI exists to prove the IPC contract: if the TUI is entirely a client
over control verbs + state stream, the GPUI app will "just work" atop the
same plumbing.

### GPUI app (target front-end)

Same client protocol; richer canvas:

- Service cards showing the full state machine as an actual diagram, with
  ports and profile membership.
- Side-by-side output panes (web's vite log + wrangler API log at once) —
  the multi-pane log compare that a single terminal TUI can't do well.
- History timeline view over the `runs`/`events` tables (restarts, failed
  builds, job runs across the day).
- Job buttons with staleness badges — the "typegen/staleness D1 reset" UI
  that stamp-file discipline deserves.

Order: TUI first (M1) as functional reference, GPUI after the engine is
stable (M4). Deliberate: engine is the risk; front-ends are cheap once the
IPC is right.

## Phase 2 (designed, not built): rust cloudflare mirror

A standalone `cf-mirror` service — a Rust/AXum server emulating the
cloudflare-worker runtime locally:

- D1 emulation on sqlite (pview's local D1 is already sqlite-on-disk inside
  `.wrangler/state/`; the mirror either wraps that or owns a sibling DB and
  applies the same migrations).
- Minimal durable-object / KV semantics needed by the app.
- Hides wrangler's ~seconds-of-boot and its own exclusive port from the
  scenario; swap-in via config:

```toml
[services.api]
provider = "cf-mirror"    # instead of "wrangler"
```

Kept out of v1 scope: it's workerd-parity risk, not orchestration risk, and
it should be its own spike once the orchestrator is stable.

## Milestones

- **M0** — engine: scenario TOML parse, providers `npm` + `exec`, PTY spawn,
  process-group stop, service state machine, runs+events sqlite, MCP server
  over socket. Usable headless + from an agent.
- **M1** — TUI over the IPC contract; wrangler + cargo providers; jobs.
- **M2** — profiles fully wired (incl. lazy), situational dep warnings,
  preflight staleness badges.
- **M3** — optional watchdog (lazy idle-stop), detached daemon hardening.
- **M4** — GPUI app.
- **M5** — CF mirror spike (separate spike branch, maybe its own doc).
