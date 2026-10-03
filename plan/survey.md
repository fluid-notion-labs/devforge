# State of Play: Local Dev Server Lifecycle Management

## Status

Survey/assessment (Oct 2026). Companion to `spec.md` (devforge) and motivated
by it: what does the current tooling landscape actually do well, where is the
gap, and why build instead of adopt. Scope is local-first — single-repo dev
scenarios, start/stop/lifecycle of local dev servers (docker included but not
the center of gravity). Remote dev environments (Coder, DevPod, Garden,
Okteto) and multi-service cluster territory are out of scope.

## Problem statement

What "managing local dev scenarios" actually requires, beyond `start
everything`:

- **Per-service lifecycle** — start/stop/restart/re-log individual services,
  not just the union.
- **Readiness, not exit** — "the port responds" and "the server said ready"
  are different from "the process spawned". Almost every tool stops at exit.
- **Dependency ordering with situational scope.**
- **State teardown & hygiene** — orphaned containers, dead ports, volumes,
  stale stamp files; the gap between "it crashed" and "it left something
  listening".
- **Code sync** — bind mounts vs watch-and-rebuild; for TS/JS usually the
  devserver's own HMR; for rust/wasm it's a rebuild step that must be noticed.
- **Build status visibility** — "is it compiling?" is the most-requested
  state in practice and almost never implemented generically.
- **Scenario switching** — different working modes need different subsets
  up ("just frontend", "with search", "with ingest"). Nobody has this as a
  first-class concept; you have shell aliases if you're lucky.
- **Onboarding reproducibility** — the other half; served mainly by
  containers, devfiles, and nix-family tools rather than lifecycle managers.

## The landscape, by approach

### 1. Raw compose + shell

`docker compose up` is the floor. It solves: reproducible environment
containers, dependency ordering (`depends_on` with `condition:
service_healthy` is genuinely fine), consistent teardown (`down`), and works
identically across your team's machines. But it notoriously does not solve:

- Per-service restart ergonomics (`restart` exists but is second-class vs
  the union up/down; log follow is terminal-per-service shells).
- Readiness surfaces are clunky (`healthcheck` works; `service_ready`-style
  eventing to shell does not).
- code-not-in-container friction is huge (bind mounts, uid matching,
  bind-mount-inotify limitations, node_modules shadowing, "is the process
  inside really the one I'm editing").
- Non-container processes (host rust, host cargo, workers spawned outside
  compose) are second-class or absent.
- Startup-time staleness detection (is the mounted source stale vs the
  image?) is not solved; rebuilds and `--build` churn are a common complaint
  (and the reason for `compose watch` existing).

**compose watch / develop.watch** is the official answer to sync: it watches
files and syncs them into the container (sync/plus rebuild/restart actions),
and has matured from awkward alpha-era to respectable. It remains
compose-specific, works within one compose file, and has nontrivial gaps
(owner/mode of watched paths, watch excludes, latency under big dep trees).
Worth caring about precisely because it suggests the "the orchestrator owns
synthetic file-watch triggers to services" model — which is exactly where a
rust orchestrator could supersede it, since vite/cargo HMR on the host
doesn't need it at all.

Verdict: the floor is solid but life above the floor is DIY. Compose is a
packaging format with a scheduler, not a lifecycle manager.

### 2. Procfile family (Foreman, Overmind, Hivemind, Honcho)

The classic polyglot answer: a Procfile listing processes, run all, TERM
whole-group on exit. Overmind & Hivemind differ only in backend (tmux vs
ptys); Overmind additionally has tmux panes so you can attach to a single
process interactively, plus per-process env file support. Honcho is
pythonic; Foreman is the origin, ruby, and strictly worst.

What they get right: careful PTY/color handling, group TERM propagation
("the whole stack dies when ctrl-c" is easy with tmux, fiddly with raw
spawn), `restart` of an individual process (Overmind has it), and no claim
that everything must be a container.

What they miss: readiness is just "process didn't exit", build status is
just stdout text, no history, no state beyond process liveness, no concept
of "profile" or "lazy" (you want search sometimes, not now; Procfile gives
one all-or-nothing up). Overmind's tmux per-pane interaction is a real
answer to "attach to one service", and is worth stealing as a UX hint for
GPUI-style panes.

Verdict: the honest baseline for most projects. Lifecycle of *processes*,
but blind to builds/state/history/profiles.

### 3. compose supersets / DX wrappers — Tilt, Skaffold, DevSpace

Tilt is the most serious of this family. It has a real resource graph
(docker_build → docker_run; k8s_apply → pods), readiness as an actual
concept, per-resource log panes, and a programmatic model (Starlark) that
lets you express "restart this when that changes". Everything above the
baseline (dependency ordering with real readiness, per-resource state in a
UI, build-status as a concept) Tilt gets *right* — that's why it's the
canonical "I have docker/k8s and want a friendly dev loop" answer.

But: today it wants a container image per service and is happiest with
k8s. For a two-service vite + wrangler local scenario, using Tilt means
inventing container images for both halves, which is exactly the wrong
shape for a repo whose reality is `host rust + host node + wrangler for
DO/D1 wiring`. Its non-container mode is a stub (`local()` resources are
single-shot commands, not long-running servers with readiness).
Skaffold/DevSpace are the same only-k8s-ward drift, with worse local-only
stories.

Verdict: closest existing implementation of the **model we want** (service
graph, readiness, build signals, per-resource UI), but constrained to the
container/k8s frame and ill-fitting for polyglot host-process repos.

### 4. Task runners / glue (make, just, mise, direnv, husky)

The honest truth of most real projects: this is what's actually running.
`just dev`, `just down`, `make up`. They solve: naming, cross-platform
syntax, stylized output. They do not try for: readiness, per-service state,
signals, history, per-service panes, profiles, agents. Used alone, they
compose with tmux of the day — and the residue is "the stuff in between",
i.e., exactly the problem: what is up, what's stale, is it ready, can
agents see it.

Verdict: not a competitor, an ingredient. A rust orchestrator should
make `just`-files a first-class call it makes, not a thing it replaces.

### 5. Declarative environments — nix family (devbox, devenv, flox)

Orthogonal but adjacent: solve "host environment reproducibility" (the
things `dev-setup.mjs` half-solves via check-then-install). devbox gives
declarative OS-level deps without nix lang; devenv does that plus
processes (devenv has a `process.<name>` concept) and is worth knowing
about as the nix-flavored idea closest to overlap. But it still doesn't do
lifecycle *state* (readiness, build status, profile lazy-ness) and doesn't
own the six other problems; its process management is more like Procfile
with nix-env attached.

Verdict: future ingredient (scenario's `[jobs.setup]` tooling could be a
devbox/devenv underneath), never the orchestrator.

### 6. devcontainers

Definitionally scoped: VS Code / Codespaces semantics, cp-launch of a
container shell with tooling pre-installed. Great for "give the agent a
clean env", poor for "run 3 host processes with live reload and per-service
TUI panes". If/when this repo wants agent-environment reproducibility, a
thin devcontainer wrapper over devforge's engine is a plausible compromise
(devforge runs inside; agent connects over MCP from outside), and the
reverse pairing (devforge starts devcontainer as a provider) is the cleaner
direction.

### 7. Edge/adjacent worth one paragraph each

- **Mirrord / Telepresence** — remote-forwarding into clusters; explicitly
  out of scope but they occupy the "Orchestrate the seam between local and
  remote" mental slot.
- **Okteto / Garden** — k8s-native dev environments; out of scope here,
  but their "sync + rebuild on change" model is the same core loop Tilt
  promotes.
- **Air (Go), nodemon, watchexec** — process *restarters* for single
  services. Useful primitives; a server-lifecycle orchestrator is exactly
  "those + graph + state/history/UI/agents".
- **PM2** — node ecosystem's babysitter; has ecosystem files, restarts,
  log panels. Haunted by: node-only worldview, deviation from modern
  lifecycle conventions, ancient CLI UX. A cautionary tale more than a
  competitor.

## Assessment: where the state of play actually is

Slicing by capability:

| Capability | compose | Procfile family | Tilt | nix/devenv | task runners | devforge (proposed) |
|---|---|---|---|---|---|---|
| per-service start/stop | meh | yes (Overmind) | yes | some | no | yes (target) |
| readiness as first-class | some | no | yes | no | no | yes (target) |
| build signals / state | no | no | some (build steps only) | no | no | yes (target) |
| profile / scenario switching | some (profiles exist) | no | no | no | no | yes (target) |
| polyglot host processes | no | yes | weak | no | yes | yes (target) |
| history / logs queryable | no | no | some | no | no | yes (sqlite, target) |
| agent interface | no | no | no | no | no | yes (MCP, target) |
| environment reproducibility | yes | no | no | yes | some | no (out) |

(Warning before this table is over-read: everything above the target column
is unfinished — compose's profiles are a real feature but not a scenario
concept, Tilt's build signals are for container builds, "history" in Tilt is
a log stream UI and not a query.)

Three things that recur as the actual missing commodity across the
landscape:

1. **A state machine per service** named as such, observable and
   controllable, reachable by both human and agent interfaces with the same
   verbs. Procfile family is liveness-blind; compose is container-lifecycle
   focused with other state second-class; Tilt is the one with the right
   idea and the wrong substrate for non-k8s repos.
2. **Build status as a lifted concept** — "compiling" as a state, with
   per-provider signal patterns, rather than as a pile of text. Nobody has
   this; the hydra of per-tool matchers is why. Capping providers solves
   the hydra while still being useful — vite/cargo/wrangler cover most
   real-world scenarios that matter here.
3. **An agent interface.** Nothing in this landscape is built with agents as
   a first-class client. `service_start(wait_for_ready=true)` — start the
   service and await readiness — as a single MCP tool call is nine layers of
   glue to replicate today (curl the port, retry, parse log, give up, report
   something useful). This genuinely doesn't exist anywhere else.

Notably absent generally: a bin that says, in one named abstraction, "in
this repo the scenario is web + API + (lazy) search, here are the state
machines, here are the profiles, here is history, and here is an agent
door." That is what devforge is proposed to be. The landscape supplies
either a subset of the capability over containers (compose, Tilt) or a
simpler subset of the capability over raw processes (Procfile family, task
runners), and nothing bridges them accumulated as one state-owned system.

## Open questions (honest)

- **Switching cost**: how many concurrent pview-style repositories hide
  enough variance that the scenario TOML turns into per-repo config debt
  before the payoff is felt? The counter: scenario files are tiny compared
  to what they replace (dev-setup glue + proc scripts + wrangler lore).
- **compose interplay**: when the rust orchestrator is good at its job, do
  you compose *inside* it (wrap docker compose as a provider) or does it
  eventually supersede compose? v1 answer: wrap as an `exec`-ish provider;
  revisit later. Don't bet against compose where compose is the right
  answer.
- **Tilt's Starlark ceiling**: could a scenario file become programmable in
  v2? Explicit no for v1; declarative shape stays simple and hydra-resistant.
- **cancel-watch**: build-state signals via output parsing are good enough;
  real tool hooks would be better. Is it worth the provider trait we avoid
  today? Deferral documented — revisit after TUI.
