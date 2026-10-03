//! Process supervision: PTY spawn, per-service state machine, process-group
//! stop (plan/spec.md "Engine" + "Service state machine"). One of the hydra
//! caps: no plugin ABI — the supervisor only knows argv + patterns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::{Mutex, broadcast::Sender as EventSender, mpsc, watch};

use crate::config::ServiceSpec;
use crate::error::{EngineError, Result};
use crate::ipc::StreamEvent;
use crate::provider::ProviderKind;
use crate::state::{ServiceState, Transition, TransitionCause};
use crate::store::Store;

/// Grace period: TERM → (grace) → KILL.
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// Cancel-watch: `starting` that never reaches `up` fails after this.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Compiling signal revert: with no further building lines in this window,
/// `compiling` goes back to `up`.
pub const COMPILING_IDLE: Duration = Duration::from_secs(3);

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One live child: state + accounting for stop/exit handling.
struct Live {
    state: ServiceState,
    pid: i32,
    run_id: i64,
    /// Exit status, `None` until the reaper thread publishes it.
    exit: watch::Receiver<Option<i32>>,
}

pub struct Supervisor {
    root: PathBuf,
    store: Arc<Store>,
    events: EventSender<StreamEvent>,
    live: Arc<Mutex<HashMap<String, Arc<Mutex<Live>>>>>,
}

impl Supervisor {
    pub fn new(root: PathBuf, store: Arc<Store>, events: EventSender<StreamEvent>) -> Self {
        Self {
            root,
            store,
            events,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record + fan out one transition; the single place both happen.
    pub async fn transition(
        &self,
        service: &str,
        from: ServiceState,
        to: ServiceState,
        cause: TransitionCause,
    ) -> Result<()> {
        let t = Transition {
            service: service.into(),
            from,
            to,
            cause,
            at_unix_ms: now_ms(),
        };
        self.store.record_transition(&t).await?;
        let _ = self.events.send(StreamEvent::Transition(t));
        Ok(())
    }

    /// Live in-memory state; absent once reaped and finalized.
    pub async fn live_state(&self, service: &str) -> Option<ServiceState> {
        let live = self.live.lock().await;
        let handle = live.get(service)?;
        Some(handle.lock().await.state)
    }

    /// Snapshot of live states, merged over store rows by the caller.
    pub async fn snapshot(&self) -> Vec<(String, ServiceState)> {
        let map = self.live.lock().await;
        let mut out = Vec::new();
        for (name, handle) in map.iter() {
            out.push((name.clone(), handle.lock().await.state));
        }
        out
    }

    /// Start `name`; `Ok(false)` = already running (idempotent start).
    pub async fn start(&self, name: &str, spec: &ServiceSpec) -> Result<bool> {
        if let Some(state) = self.live_state(name).await
            && matches!(
                state,
                ServiceState::Starting | ServiceState::Up | ServiceState::Compiling
            )
        {
            return Ok(false);
        }
        let provider = ProviderKind::from_spec(spec);
        let cwd = self.root.join(spec.cwd.as_deref().unwrap_or(""));
        let argv = provider.argv(spec, None).ok_or_else(|| EngineError::Ipc {
            message: format!("service `{name}` has neither `script` (npm) nor `command`"),
        })?;

        self.transition(
            name,
            ServiceState::Idle,
            ServiceState::Starting,
            TransitionCause::UserRequest,
        )
        .await?;

        match self.spawn(name, provider, &argv, &cwd, spec).await {
            Ok(()) => Ok(true),
            Err(e) => {
                self.transition(
                    name,
                    ServiceState::Starting,
                    ServiceState::Failed,
                    TransitionCause::Timeout,
                )
                .await?;
                Err(e)
            }
        }
    }

    async fn spawn(
        &self,
        name: &str,
        provider: ProviderKind,
        argv: &[String],
        cwd: &Path,
        spec: &ServiceSpec,
    ) -> Result<()> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)?;

        let mut command = CommandBuilder::new(argv[0].clone());
        for arg in &argv[1..] {
            command.args([arg.clone()]);
        }
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| EngineError::Ipc {
                message: format!("spawn `{name}`: {e}"),
            })?;
        let pid = i32::try_from(child.process_id().unwrap_or(0)).unwrap_or(0);

        let run_id = self
            .store
            .record_run_start(
                name.to_string(),
                None,
                Some(pid),
                cwd.display().to_string(),
                argv.join(" "),
                now_ms(),
            )
            .await?;
        self.store
            .append_log(name.to_string(), format!("$ {}", argv.join(" ")), now_ms())
            .await?;

        // PTY bytes → log lines → sqlite + state machine. Own thread, since
        // the reader is a plain std Read.
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| EngineError::Ipc {
                message: format!("pty reader: {e}"),
            })?;
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']).to_owned();
                        if !trimmed.is_empty() && line_tx.send(trimmed).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        // Lines keep flowing through the cloned reader; dropping the master
        // lets EOF propagate once the child closes its slave end.
        drop(pair.master);

        // Blocking reap thread: publishes the exit code when the child dies.
        let (exit_tx, exit_rx) = watch::channel(None::<i32>);
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            drop(child);
            let _ = exit_tx.send(code);
        });

        let live = Arc::new(Mutex::new(Live {
            state: ServiceState::Starting,
            pid,
            run_id,
            exit: exit_rx.clone(),
        }));
        self.live
            .lock()
            .await
            .insert(name.to_string(), live.clone());

        self.spawn_line_consumer(
            name.to_owned(),
            provider,
            line_rx,
            spec.ready_when.clone(),
            live.clone(),
        );
        self.spawn_exit_watch(name.to_owned(), live.clone());
        self.spawn_start_watch(name.to_owned(), live);
        Ok(())
    }

    /// Log-line consumer: drains the PTY producer and runs the pattern state
    /// machine. Ends when the producer channel closes (child exit).
    fn spawn_line_consumer(
        &self,
        name: String,
        provider: ProviderKind,
        mut rx: mpsc::UnboundedReceiver<String>,
        ready_when: Option<String>,
        live: Arc<Mutex<Live>>,
    ) {
        let store = self.store.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let _ = store.append_log(name.clone(), line.clone(), now_ms()).await;
                match line_match(&line, provider, ready_when.as_deref()) {
                    Some(Signal::Ready { pattern }) => {
                        let state = live.lock().await.state;
                        if state == ServiceState::Starting {
                            live.lock().await.state = ServiceState::Up;
                            transition_v(
                                &store,
                                &events,
                                &name,
                                ServiceState::Starting,
                                ServiceState::Up,
                                TransitionCause::ProviderPattern { pattern },
                            )
                            .await;
                        }
                    }
                    Some(Signal::Compiling { signal }) => {
                        let _ = events.send(StreamEvent::BuildSignal {
                            service: name.clone(),
                            signal: signal.clone(),
                        });
                        let state = live.lock().await.state;
                        if state == ServiceState::Up {
                            live.lock().await.state = ServiceState::Compiling;
                            transition_v(
                                &store,
                                &events,
                                &name,
                                ServiceState::Up,
                                ServiceState::Compiling,
                                TransitionCause::ProviderPattern { pattern: signal },
                            )
                            .await;
                        }
                        if state == ServiceState::Compiling {
                            schedule_revert_up(
                                store.clone(),
                                events.clone(),
                                name.clone(),
                                live.clone(),
                            );
                        }
                    }
                    Some(Signal::Stable { signal }) => {
                        let _ = events.send(StreamEvent::BuildSignal {
                            service: name.clone(),
                            signal: signal.clone(),
                        });
                        let state = live.lock().await.state;
                        if state == ServiceState::Compiling {
                            live.lock().await.state = ServiceState::Up;
                            transition_v(
                                &store,
                                &events,
                                &name,
                                ServiceState::Compiling,
                                ServiceState::Up,
                                TransitionCause::ProviderPattern { pattern: signal },
                            )
                            .await;
                        }
                    }
                    Some(Signal::Failure { signal }) => {
                        let _ = events.send(StreamEvent::BuildSignal {
                            service: name.clone(),
                            signal,
                        });
                    }
                    None => {}
                }
            }
        });
    }

    /// Exit finalizer: closes the run out, lands the terminal state
    /// (`idle` after a stop, `failed` on a crash) and removes the live entry.
    /// All exit finalization lives here so it cannot double-run.
    fn spawn_exit_watch(&self, name: String, live: Arc<Mutex<Live>>) {
        let store = self.store.clone();
        let events = self.events.clone();
        let live_map = self.live.clone();
        tokio::spawn(async move {
            let (run_id, mut exit) = {
                let l = live.lock().await;
                (l.run_id, l.exit.clone())
            };
            // Wait for the reap (changed() also fires when the sender drops).
            while exit.changed().await.is_ok() && exit.borrow().is_none() {
                // keep waiting
            }
            let code = exit.borrow().as_ref().copied();

            // Remove the live entry; if some other path got here first, skip.
            if live_map.lock().await.remove(&name).is_none() {
                return;
            }
            let state = live.lock().await.state;
            let to = if state == ServiceState::Stopping || code == Some(0) {
                ServiceState::Idle
            } else {
                ServiceState::Failed
            };
            let cause = if state == ServiceState::Stopping {
                TransitionCause::UserRequest
            } else {
                TransitionCause::Crash { exit_code: code }
            };
            if state != to {
                transition_v(&store, &events, &name, state, to, cause).await;
            }
            let _ = store.record_run_end(run_id, now_ms(), code).await;
            live.lock().await.state = to;
        });
    }

    /// Cancel-watch: `starting` never reaching `up` fails after the timeout,
    /// with the process group killed.
    fn spawn_start_watch(&self, name: String, live: Arc<Mutex<Live>>) {
        let store = self.store.clone();
        let events = self.events.clone();
        let live_map = self.live.clone();
        tokio::spawn(async move {
            tokio::time::sleep(START_TIMEOUT).await;
            let (state, pid, run_id) = {
                let l = live.lock().await;
                (l.state, l.pid, l.run_id)
            };
            if state == ServiceState::Starting {
                transition_v(
                    &store,
                    &events,
                    &name,
                    ServiceState::Starting,
                    ServiceState::Failed,
                    TransitionCause::Timeout,
                )
                .await;
                group_kill(pid, libc::SIGKILL);
                let _ = store.record_run_end(run_id, now_ms(), None).await;
                live.lock().await.state = ServiceState::Failed;
                live_map.lock().await.remove(&name);
            }
        });
    }

    /// Stop a service: record `stopping`, TERM the process group, grace wait,
    /// then KILL. `Ok(false)` when the service is not running.
    pub async fn stop(&self, name: &str) -> Result<bool> {
        let Some(live) = self.live.lock().await.get(name).cloned() else {
            return Ok(false);
        };
        let (state, pid) = {
            let l = live.lock().await;
            (l.state, l.pid)
        };
        if state == ServiceState::Stopping {
            return Ok(true);
        }
        self.transition(
            name,
            state,
            ServiceState::Stopping,
            TransitionCause::UserRequest,
        )
        .await?;
        live.lock().await.state = ServiceState::Stopping;
        group_kill(pid, libc::SIGTERM);
        // The exit finalizer removes the live entry when the child is reaped.
        if !self.wait_reaped(name, STOP_GRACE).await {
            group_kill(pid, libc::SIGKILL);
            self.wait_reaped(name, Duration::from_secs(5)).await;
        }
        Ok(true)
    }

    /// True once the live entry for `name` disappears (finalization done).
    async fn wait_reaped(&self, name: &str, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if !self.live.lock().await.contains_key(name) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        !self.live.lock().await.contains_key(name)
    }

    pub async fn stop_all(&self) -> Result<()> {
        let names: Vec<String> = self.live.lock().await.keys().cloned().collect();
        for name in names {
            self.stop(&name).await?;
        }
        Ok(())
    }
}

fn group_kill(pid: i32, sig: i32) {
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

fn io_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Ipc {
        message: e.to_string(),
    }
}

#[derive(Debug, PartialEq)]
enum Signal {
    Ready { pattern: String },
    Compiling { signal: String },
    Failure { signal: String },
    Stable { signal: String },
}

/// Provider-pattern signal classification (v1: pattern heuristics only).
/// An explicit `ready_when` always wins over the provider defaults. Failure
/// patterns emit `Failure` signals only — actual `failed` transitions come
/// from the (nonzero) exit, keeping the state machine honest.
fn line_match(line: &str, provider: ProviderKind, ready_when: Option<&str>) -> Option<Signal> {
    if let Some(pattern) = ready_when.filter(|p| line.contains(p)) {
        return Some(Signal::Ready {
            pattern: pattern.to_string(),
        });
    }
    match provider {
        ProviderKind::Npm => {
            // vite is the npm dev server of record here
            if line.contains("ready in") {
                return Some(Signal::Ready {
                    pattern: "ready in (vite)".into(),
                });
            }
            if line.contains("error during build") || line.contains("Internal server error") {
                return Some(Signal::Failure {
                    signal: "vite build error".into(),
                });
            }
            if line.contains("hmr update") {
                return Some(Signal::Compiling {
                    signal: "vite hmr".into(),
                });
            }
            None
        }
        ProviderKind::Wrangler => {
            if line.contains("Ready on http") {
                return Some(Signal::Ready {
                    pattern: "Ready on http (wrangler)".into(),
                });
            }
            let lower = line.to_ascii_lowercase();
            if lower.contains("watching for file changes") {
                return Some(Signal::Compiling {
                    signal: "wrangler reload".into(),
                });
            }
            None
        }
        ProviderKind::Cargo => {
            if line.starts_with("error[") || line.starts_with("error:") {
                return Some(Signal::Failure {
                    signal: "cargo compile error".into(),
                });
            }
            if line.starts_with("Compiling ") {
                return Some(Signal::Compiling {
                    signal: "cargo compiling".into(),
                });
            }
            if line.starts_with("Finished ") {
                return Some(Signal::Stable {
                    signal: "cargo finished".into(),
                });
            }
            None
        }
        ProviderKind::Exec => None,
    }
}

/// Revert `compiling → up` after the idle window if nothing re-entered.
fn schedule_revert_up(
    store: Arc<Store>,
    events: EventSender<StreamEvent>,
    name: String,
    live: Arc<Mutex<Live>>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(COMPILING_IDLE).await;
        if live.lock().await.state == ServiceState::Compiling {
            live.lock().await.state = ServiceState::Up;
            transition_v(
                &store,
                &events,
                &name,
                ServiceState::Compiling,
                ServiceState::Up,
                TransitionCause::Timeout,
            )
            .await;
        }
    });
}

async fn transition_v(
    store: &Arc<Store>,
    events: &EventSender<StreamEvent>,
    name: &str,
    from: ServiceState,
    to: ServiceState,
    cause: TransitionCause,
) {
    let t = Transition {
        service: name.into(),
        from,
        to,
        cause,
        at_unix_ms: now_ms(),
    };
    if let Err(e) = store.record_transition(&t).await {
        tracing::warn!("transition record failed: {e}");
    }
    let _ = events.send(StreamEvent::Transition(t));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m2(provider: ProviderKind, line: &str) -> Option<Signal> {
        line_match(line, provider, None)
    }
    fn m(provider: ProviderKind, line: &str, ready: Option<&str>) -> Option<Signal> {
        line_match(line, provider, ready)
    }

    #[test]
    fn explicit_ready_pattern_wins() {
        assert!(m(ProviderKind::Exec, "listening on 9000", Some("listening")).is_some());
        assert_eq!(m(ProviderKind::Exec, "listening", Some("ready")), None);
    }

    #[test]
    fn npm_vite_patterns() {
        assert!(matches!(
            m(ProviderKind::Npm, "  ➜  Local:  ready in 300 ms", None),
            Some(Signal::Ready { .. })
        ));
        assert!(matches!(
            m(ProviderKind::Npm, "hmr update /src/main.ts", None),
            Some(Signal::Compiling { .. })
        ));
        assert!(matches!(
            m(ProviderKind::Npm, "error during build:", None),
            Some(Signal::Failure { .. })
        ));
    }

    #[test]
    fn wrangler_patterns() {
        assert!(matches!(
            m(
                ProviderKind::Wrangler,
                "Ready on http://127.0.0.1:8787",
                None
            ),
            Some(Signal::Ready { .. })
        ));
        assert!(matches!(
            m(ProviderKind::Wrangler, "Watching for file changes...", None),
            Some(Signal::Compiling { .. })
        ));
    }

    #[test]
    fn cargo_finished_is_stable() {
        assert!(matches!(
            m2(ProviderKind::Cargo, "Finished dev [unoptimized] target(s)"),
            Some(Signal::Stable { .. })
        ));
    }

    #[test]
    fn cargo_patterns() {
        assert!(matches!(
            m(ProviderKind::Cargo, "Compiling devforge-core v0.1.0", None),
            Some(Signal::Compiling { .. })
        ));
        assert!(matches!(
            m(ProviderKind::Cargo, "error[E0432]: unresolved import", None),
            Some(Signal::Failure { .. })
        ));
        assert!(matches!(
            m(ProviderKind::Cargo, "Finished dev profile", None),
            Some(Signal::Stable { .. })
        ));
    }
}
