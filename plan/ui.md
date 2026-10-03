# devforge-tui — UI Plan (M1)

**Scope:** ratatui front-end, pure IPC client of `devforge_core::ipc`
(control verbs + `StreamEvent` stream). No engine internals; if the TUI wants
something the contract doesn't offer, the contract is wrong.

**Status of existing code:** `crates/devforge-tui/src/lib.rs` already has a
skeleton — `TuiModel` / `ServiceRow`, a two-pane layout (services left 40%,
logs right 60%), a one-line status bar, `j/k` selection, `q`/`Ctrl-c` quit,
state-dot colors. The IPC socket, verb replies, and stream consumption are
`TODO(M1)`. This plan builds on that skeleton.

> **Deferred:** the log pane's UI will play out from
> [vendor/narwhal](../vendor/narwhal) (our own burst-sealing tab-style log
> watcher). Folding its sections/tabs + follow semantics into the pane
> deserves a dedicated revision/pass — planned at a later point (see
> `index.md` → Vendored references).

---

## 1. Visual language (derived from vendor/pview)

pview's UI (`App.svelte`, `VenuePage.svelte`, `app.css`) is deliberately
quiet: a single `h1`/`h2` title, a **status dot next to the title**
(`.status[data-status]` — green when open, `#c05621` amber-orange when
reconnecting/closed, otherwise dimmed), **muted helper text** (`opacity:
0.7`) for anything situational, **banners** with a thin colored frame for
errors ("Connection lost. Reconnecting…"), and lowercase single-word
statuses (`open`, `reconnecting`, `idle`, `live`). Selection is a filled
button (`.on` — blue `#2b6cb0`).

TUI translation of those conventions:

| pview convention | TUI convention |
|---|---|
| status dot with value | state dot `●` colored: gray idle, yellow starting/compiling, green up, red failed, dark-gray stopping (verbatim from spec, already in code) |
| muted text | `Modifier::DIM` (DarkGray fg) for hints, secondary columns, empty-state text |
| error banner, thin `#c0562166` border | a rounded top banner line framed in the accent color (yellow) across the active pane |
| `.on` blue selection | selected row IS REVERSED+BOLD (already in code); toggles in overlays styled `[ x ]` vs `[   ]` |
| lowercase one-word states | `idle`, `starting`, `up`, `compiling`, `failed`, `stopping` exactly — the state machine vocabulary |
| "Is the worker running?" hint | error state always suggests the fix ("is the daemon running? `devforge serve`") |
| `?venue=rileys` seed realism | all mockups below use the pview scenario (web/api/search/markserv, setup/db_reset jobs) |

No borders-on-borders: pview's layout is whitespace-driven, so panes get
single thin borders only where they're primary; the status bar and banners
are unframed.

## 2. Global layout

```
┌─ total frame ──────────────────────────────────────────────────┐
│ (optional) banner row          — 1 hl, only when a toast/error │
│ main area  ──────────────────── left 40% / right 60% (existing)│
│ status bar                     — 1 row, unframed               │
└────────────────────────────────────────────────────────────────┘
```

Overlays (help, job runner, profile picker, confirm dialogs, actions like
`s`/`x`/`r` require confirmation via a centered modal rather than firing)
are centered popups drawn last; modal = thin bordered block, black backing.
Every overlay closes with `Esc`.

Left pane: services (from `scenario_status` / stream). Right pane: tabs for
the selected service's tail and one for jobs' output. Tab switch deviates
from the spec's `L`-tails only in that `L` still re-pulls `service_logs`.

State dot legend (mirrors the spec), rendered once in the footer legend:
`● idle  ● up  compiling `; the dot column prints the state word itself,
pview-style "dot + word in muted color".

## 3. Screens — ASCII mockups

### 3.1 Main dashboard — "all" profile live

The canonical screen. Left service list, right log pane for the selected
service.

```text
┌ services ──────────────────────────┐  ┌ logs — web ────────────────────────────────┐
│                                    │  │                                            │
│      web  :5173  ● up   vite 413ms │  │ 08:12:01 ● VITE v7.0.3  ready in 413 ms    │
│      api  :8787  ● up   Ready on … │  │                                            │
│                                    │  │   ➜  Local:   http://localhost:5173/       │
│                                    │  │   ➜  Network: http://192.168.0.10:5173/    │
│                                    │  │ 08:12:44 ● [vite] hmr update /src/App…    │
│                                    │  │ 08:13:02 ● [vite] page reload /api        │
│                                    │  │                                            │
│                                    │  │                                            │
│                                    │  │                                            │
│                                    │  │                                            │
└────────────────────────────────────┘  └────────────────────────────────────────────────┘
 p:all · 2/2 up · jobs: setup ● , db_reset ──  s:1 s  x:1 x  r:1 r  L:1 tails  j:1 jobs  p:1 p P…
```

Notes: state word is green when up — with the current profile — and on the
right, preflight staleness badge `setup ●` (a stale job gets a red `●`;
fresh gets nothing dimmed, not-fetched gets `—`, mirroring pview's
`(rev n)`-muted style). Truncate pane→pane: left detail lines one per
service; right log with horizontal scroll to disable striping — hmr lines
merge when the terminal is narrow.

### 3.2 Job runner overlay

```text
┌─ jobs ────────────────────────┐
│ ► setup    last ok 3m · ok    │
│   db_reset last 2h · ok       │
│───────────────────────────────│
│ [setup — pview]               │
│ node scripts/dev-setup.mjs    │
│                                │
│ deps: ok (unchanged)          │
│ wasm-pack: ok (unchanged)     │
│ ts-rs types: wrote 8 files    │
│ D1 migrate: skip (0 pending)  │
│ .dev.vars: ok (unchanged)     │
│                                │
│ [r run]  [esc close]          │
└───────────────────────────────┘
```

Title = job name; job state lives in the TUI model only — for jobs there
are two targeted verbs, no re-derivation.

### 3.3 Help overlay (`?`)

The pview UI never explains; help overlay is the only centering pane,
single-column key map, muted values. Each key on its own line:

```text
┌─ help ────────────────────────────────────────────┐
│ j/k / arrows      select service                  │
│ s                 start selected (confirm)        │
│ x                 stop selected (confirm)         │
│ r                 restart selected (confirm)      │
│ L                 re-tail selected's log          │
│ j                 job runner overlay              │
│ p/P               cycle profile / profile picker  │
│ tab               right-pane log/jobs             │
│ ?                 help                            │
│ q / Ctrl-c        quit                            │
│                                                    │
│ profiles: all · scratch · ingest (p to switch)     │
│ lazy: search, markserv live in idle until started  │
└────────────────────────────────────────────────────┘
```

### 3.4 Error / empty states

pview's error banner pattern, "Is the worker running?" suggestion included
explicitly:

```text
────────────────────────────────────────────────────────────────────────────
──────  daemon connection lost — verbs queue; reconnecting   [esc: clear] ─-
────────────────────────────────────────────────────────────────────────────
┌ services ──────────────────┐  ┌ logs ─────────────────────────────────────┐
│  web   ● up    —           │  │                                            │
│  api   ● up    —           │  │   (daemon disconnected)                    │
│ search ● idle  lazy        │  │   last snapshot kept; state may be stale   │
│ markse ● idle  lazy        │  │                                            │
└────────────────────────────┘  └────────────────────────────────────────────┘
 daemon: disconnected · last seen 08:14:02                          press ? for…
```

Empty states ("no scenario loaded", "socket missing") reuse the same
pattern: `socket not found at .devforge/socket — did you run devforge
serve?`.

### 3.5 Startup / connecting

`EnterAlternateScreen` then draw immediately (existing `TuiModel::default`
shows empty). First frame shows a connect spinner — pview's `{#await}`
"Loading rileys…" line, muted:

```text
┌ services ──────┐  ┌ logs ─────────────────────────────────────┐
│                │  │                                            │
│ (no scenario)  │  │   connecting to .devforge/socket …          │
│                │  │                                            │
```

## 4. Data flow (IPC verbs → model)

Model is a single `Store` (pview analog: `VenueStore`) fed by two input
channels — verb replies and stream pushes — with the contract interior to
the TUI:

- **keys of the model:** `services: IndexMap<String, ServiceRow>`
  (IndexMap preserves scenario-file order — the reason it's already a
  workspace dep), `profile: Option<String>`, `profiles: Vec<String>`,
  `jobs: Map<JobName, JobStatus + staleness>`,
  `log_ring: HashMap<Service, VecDeque<Line>>` (cap 5000/pane),
  `toasts: Vec<Toast>`, `connected: bool`.
- **on boot:** connect socket → send `scenario_status` → populate
  services/profiles/jobs/staleness → subscribe to the state stream; if the
  verb fails, show the "daemon: connecting…" error banner.
- **stream pushes (`StreamEvent`):** state transitions update
  `services[name].state` (+ transition cause stamp); `build_signal` events
  update the compiling badge and last-line; `log_line` events append to the
  matching ring (if the pane is following, keep `follow=true` — the pview
  "Follow director" checkbox becomes `follow: bool` per pane, toggled by
  `f`, so a manual PgUp unpins the tail and a toast "unfollow (f to
  re-follow)" is consistent with pview's pick/follow semantics); job status
  events update the badges. Redraw is event-driven (per existing loop +
  `event::poll`), no polling timer except reconnect.
- **key presses:** produce a `Verb` on the existing mpsc channel (already
  typed `Tx`): `service_start/restart/stop(name, wait)`, `job_run(name)`,
  `scenario_start/stop(profile)`, `service_logs(name, tail)`,
  `scenario_reload`. Verb replies update model panes immediately; a reply
  error becomes a toast, not a modal (pview error-as-row style).
- **Failure posture:** nothing in the TUI opens sqlite, reads scenario
  TOML, or learns provider internals — everything arrives as payload data
  from `scenario_status` / stream. Any field a screen wants and the stream
  lacks gets filed as a contract change (e.g., "job staleness badge" lives
  in the verb response payload now, not a TUI re-derivation).

### Keybinding summary (final)

```
j/k ↑↓  select service          s/x/r  start/stop/restart (confirm modal)
L       re-tail service log     j      jobs overlay (run with enter)
p/P     profile picker/cycle    f      follow log tail (toggle)
tab     right pane: log/jobs    ?      help
R       scenario_reload        q/Ctrl-c quit
```

## 5. Milestone mapping (M1 ± M2, per spec & index.md)

- **D1:** socket connect, verb send/reply, stream subscribe, `TuiModel`
  store, follow-tail log pane. (Unlock: everything else.)
- **D2:** key actions with confirm modal; jobs overlay; toasts.
- **D3 (M2 territory):** profile picker + lazy service handling + staleness
  badges in the footer; situational dep warning toast (e.g., start web alone
  → "api not started — advisory dep in profile 'all'").
- **Late:** reconnect banner with queued verbs, ANSI passthrough in the log
  pane (PTY keeps colors; render source lines via `vt100` or a minimal
  ANSI-to-ratatui-text converter — a cost note for
  `docs/architecture.md`, not decided here).
