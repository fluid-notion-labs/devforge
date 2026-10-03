//! Engine daemon: IPC server binding the local socket, dispatching verbs and
//! fanning `StreamEvent`s. Seat of truth; front-ends connect over the socket (plan/spec.md).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{self, Scenario, after_order};
use crate::error::{EngineError, Result};
use crate::ipc::{DEFAULT_SOCKET_PATH, Reply, Verb};
use crate::state::ServiceState;
use crate::store::Store;
use crate::supervisor::{Supervisor, now_ms};

/// The engine: scenario config + sqlite store + supervisor, shared by all
/// IPC connections.
pub struct Engine {
    pub root: PathBuf,
    scenario: RwLock<Scenario>,
    store: Arc<Store>,
    sup: Arc<Supervisor>,
    events: tokio::sync::broadcast::Sender<crate::ipc::StreamEvent>,
}

impl Engine {
    /// Open the store under `root/.devforge/` and load the scenario.
    pub async fn open(root: PathBuf) -> Result<Self> {
        let scenario = config::load(&root)?;
        let store = Arc::new(Store::open(&root.join(".devforge/state.sqlite3")).await?);
        info!(root = %root.display(), name = %scenario.scenario.name, "engine open");
        let events = tokio::sync::broadcast::channel(256).0;
        let sup = Arc::new(Supervisor::new(root.clone(), store.clone(), events.clone()));
        Ok(Self {
            root,
            scenario: RwLock::new(scenario),
            store,
            sup,
            events,
        })
    }

    /// Emit a stream event to all subscribers (stream channel).
    pub fn emit(&self, event: crate::ipc::StreamEvent) {
        // 0 subscribers is normal (headless); dropped events are expected churn.
        let _ = self.events.send(event);
    }

    /// Re-read the scenario TOML, keeping state (config::load falls back to an
    /// empty default; a parse error keeps the previous scenario).
    pub async fn reload(&self) -> Result<()> {
        match config::load(&self.root) {
            Ok(s) => {
                *self.scenario.write().await = s;
                Ok(())
            }
            Err(crate::error::EngineError::ScenarioRead { .. }) => Err(EngineError::Ipc {
                message: "scenario reload failed: no scenario file found".into(),
            }),
            Err(EngineError::ScenarioParse { path, source }) => Err(EngineError::Ipc {
                message: format!("scenario reload failed ({}): {source}", path.display()),
            }),
            Err(e) => Err(e),
        }
    }

    /// Socket path: `[mcp].socket` override, else `.devforge/socket`.
    pub async fn socket_path(&self) -> PathBuf {
        self.scenario
            .read()
            .await
            .mcp
            .socket
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.join(DEFAULT_SOCKET_PATH))
    }

    /// Bind the socket and serve verbs until the process dies.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let socket_path = self.socket_path().await;
        let listener = bind_socket(&socket_path).await?;
        info!(socket = %socket_path.display(), "IPC listening");

        loop {
            let (stream, _addr) = listener.accept().await.map_err(|e| EngineError::Ipc {
                message: e.to_string(),
            })?;
            let engine = self.clone();
            tokio::spawn(async move {
                if let Err(e) = engine.handle_conn(stream).await {
                    warn!("connection dropped: {e}");
                }
            });
        }
    }

    /// One client connection: newline-delimited JSON `Verb` in, `Reply` out.
    async fn handle_conn(self: Arc<Self>, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.map_err(io_err)? {
            let reply = match serde_json::from_str::<Verb>(&line) {
                Ok(Verb::Subscribe {}) => {
                    let mut out = serde_json::to_vec(&Reply::Ok(json!({ "subscribed": true })))
                        .map_err(EngineError::from)?;
                    out.push(b'\n');
                    writer.write_all(&out).await.map_err(io_err)?;
                    return self.pump_stream(writer).await;
                }
                Ok(verb) => self.dispatch(verb).await,
                Err(e) => Reply::Err {
                    message: format!("bad verb: {e}"),
                },
            };
            let mut out = serde_json::to_vec(&reply).map_err(EngineError::from)?;
            out.push(b'\n');
            writer.write_all(&out).await.map_err(io_err)?;
        }
        Ok(())
    }
    /// After `Subscribe`: push StreamEvents to this writer until the client leaves.
    async fn pump_stream(&self, mut writer: tokio::net::unix::OwnedWriteHalf) -> Result<()> {
        let mut rx = self.events.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let mut line = serde_json::to_vec(&event).map_err(EngineError::from)?;
                    line.push(b'\n');
                    writer.write_all(&line).await.map_err(io_err)?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("stream subscriber lagged, {n} events skipped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }

    /// One verb → one reply.
    pub async fn dispatch(&self, verb: Verb) -> Reply {
        match verb {
            Verb::ScenarioStatus => match self.status().await {
                Ok(v) => Reply::Ok(v),
                Err(e) => e.into_reply(),
            },
            Verb::ScenarioStart { profile } => {
                if let Some(p) = &profile
                    && !self.scenario.read().await.profiles.contains_key(p)
                {
                    return EngineError::UnknownProfile { name: p.clone() }.into_reply();
                }
                self.start_profile(profile).await
            }
            Verb::ScenarioReload => match self.reload().await {
                Ok(()) => Reply::Ok(json!({ "reloaded": true })),
                Err(e) => e.into_reply(),
            },
            Verb::ServiceLogs { name, tail } => {
                if !self.known_service(&name).await {
                    return EngineError::UnknownService { name }.into_reply();
                }
                match self.store.service_logs(name, tail.unwrap_or(100)).await {
                    Ok(rows) => Reply::Ok(json!({ "lines": rows })),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::EventQuery { filter } => match self.event_query(filter).await {
                Ok(v) => Reply::Ok(v),
                Err(e) => e.into_reply(),
            },
            Verb::ServiceStart {
                name,
                wait_for_ready,
            } => {
                if !self.known_service(&name).await {
                    return EngineError::UnknownService { name }.into_reply();
                }
                match self.start_service(&name, wait_for_ready).await {
                    Ok(state) => Reply::Ok(state),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::ServiceRestart { name } => {
                if !self.known_service(&name).await {
                    return EngineError::UnknownService { name }.into_reply();
                }
                let _ = self.sup.stop(&name).await;
                match self.start_service(&name, None).await {
                    Ok(state) => Reply::Ok(state),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::ServiceStop { name } => {
                if !self.known_service(&name).await {
                    return EngineError::UnknownService { name }.into_reply();
                }
                match self.sup.stop(&name).await {
                    Ok(started) => Reply::Ok(json!({ "stopped": started })),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::ScenarioStop => match self.sup.stop_all().await {
                Ok(()) => Reply::Ok(json!({ "stopped": true })),
                Err(e) => e.into_reply(),
            },
            Verb::JobRun { name } => {
                if !self.scenario.read().await.jobs.contains_key(&name) {
                    return EngineError::UnknownJob { name }.into_reply();
                }
                match self.run_job(&name).await {
                    Ok(v) => Reply::Ok(v),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::NpmScripts { name } => {
                if !self.known_service(&name).await {
                    return EngineError::UnknownService { name }.into_reply();
                }
                match self.npm_scripts(&name).await {
                    Ok(v) => Reply::Ok(v),
                    Err(e) => e.into_reply(),
                }
            }
            Verb::Subscribe {} => Reply::Ok(json!({ "subscribed": true })),
        }
    }

    /// Start a service, optionally blocking until `up`/`failed`.
    async fn start_service(&self, name: &str, wait_for_ready: Option<bool>) -> Result<Value> {
        let spec = self.scenario.read().await.services.get(name).cloned();
        let Some(spec) = spec else {
            return Err(EngineError::UnknownService {
                name: name.to_string(),
            });
        };
        let spawned = self.sup.start(name, &spec).await?;
        let state = self
            .sup
            .live_state(name)
            .await
            .unwrap_or(ServiceState::Idle);
        if wait_for_ready != Some(true) {
            return Ok(json!({ "started": spawned, "state": state }));
        }
        // Block until up/failed (the cancel-watch enforces the same timeout).
        for _ in 0..300 {
            if let Some(state) = self.sup.live_state(name).await
                && matches!(state, ServiceState::Up | ServiceState::Failed)
            {
                return Ok(json!({ "state": state }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(json!({ "state": "timeout" }))
    }

    /// Enumerate the scripts object of the service's `package.json`
    /// (npm/wrangler providers).
    async fn npm_scripts(&self, name: &str) -> Result<Value> {
        let spec = self.scenario.read().await.services.get(name).cloned();
        let Some(spec) = spec else {
            return Err(EngineError::UnknownService {
                name: name.to_string(),
            });
        };
        let cwd = self.root.join(spec.cwd.as_deref().unwrap_or(""));
        let pkg = cwd.join("package.json");
        let text = std::fs::read_to_string(&pkg).map_err(|source| EngineError::ScenarioRead {
            path: pkg.clone(),
            source,
        })?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EngineError::Ipc {
                message: format!("{}: {e}", pkg.display()),
            })?;
        let scripts = json
            .get("scripts")
            .and_then(|s| s.as_object())
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(json!({ "scripts": scripts }))
    }

    /// Start a profile: named set, or the default (every non-`lazy` service
    /// in config order). Advisory `after` edges reorder the start sequence
    /// within the set — missing companions are warnings, never surprise boots.
    async fn start_profile(&self, profile: Option<String>) -> Reply {
        let scenario = self.scenario.read().await;
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        let names: Vec<String> = match &profile {
            Some(p) => match scenario.profiles.get(p) {
                Some(list) => list.clone(),
                None => {
                    return EngineError::UnknownProfile { name: p.clone() }.into_reply();
                }
            },
            None => scenario
                .services
                .iter()
                .filter(|(_, s)| !s.lazy)
                .map(|(k, _)| k.clone())
                .collect(),
        };

        // Unknown names become errors; drop them from the set.
        let set: Vec<String> = names
            .into_iter()
            .filter(|n| {
                if scenario.services.contains_key(n) {
                    true
                } else {
                    errors.push(format!("profile names unknown service `{n}`"));
                    false
                }
            })
            .collect();

        // Advisory `after`: order within the set; warn about companions absent.
        let ordered = after_order(&set, &scenario.services);
        for name in &ordered {
            let Some(spec) = scenario.services.get(name) else {
                continue;
            };
            for dep in &spec.after {
                if !set.contains(dep) && scenario.services.contains_key(dep) {
                    warnings.push(format!(
                        "service `{name}` prefers `{dep}` — not in this profile, not starting it"
                    ));
                }
            }
        }

        let mut started = Vec::new();
        for name in ordered {
            // Config guarantees these exist (filtered above).
            let spec = scenario.services.get(&name).expect("set filtered");
            match self.sup.start(&name, spec).await {
                Ok(_) => started.push(name),
                Err(e) => errors.push(e.to_string()),
            }
        }
        Reply::Ok(json!({ "started": started, "warnings": warnings, "errors": errors }))
    }

    /// Run a `[jobs.*]` entry to completion: shell-free argv, output tailed
    /// into `events` as `log_line` rows, result flagged as `job_result`.
    async fn run_job(&self, name: &str) -> Result<Value> {
        let job = self.scenario.read().await.jobs.get(name).cloned();
        let Some(job) = job else {
            return Err(EngineError::UnknownJob {
                name: name.to_string(),
            });
        };
        let cwd = self.root.join(job.cwd.as_deref().unwrap_or(""));
        let mut argv = shell_words::split(&job.command).map_err(|e| EngineError::Ipc {
            message: format!("job `{name}` argv: {e}"),
        })?;
        if argv.is_empty() {
            return Err(EngineError::Ipc {
                message: format!("job `{name}` has empty command"),
            });
        }
        let (prog, args) = (argv.remove(0), argv);

        tracing::info!(job = %name, "running job");
        let output = tokio::process::Command::new(prog)
            .args(&args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| EngineError::Ipc {
                message: format!("job `{name}` spawn: {e}"),
            })?;

        // Interleave stdout/stderr in order of arrival; combine for the tail.
        let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
        out.push_str(&String::from_utf8_lossy(&output.stderr));

        let tail: Vec<String> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect();
        let tail_last: Vec<String> = tail[tail.len().saturating_sub(20)..].to_vec();
        for line in &tail {
            self.store
                .append_event(Some(name.to_string()), "log_line", line.clone(), now_ms())
                .await?;
        }
        self.store
            .append_event(
                Some(name.to_string()),
                "job_result",
                serde_json::to_string(&json!({ "exit_code": output.status.code() }))?,
                now_ms(),
            )
            .await?;

        Ok(json!({
            "exit_code": output.status.code(),
            "success": output.status.success(),
            "tail": tail_last,
        }))
    }

    async fn known_service(&self, name: &str) -> bool {
        self.scenario.read().await.services.contains_key(name)
    }

    /// ScenarioStatus payload: config merged with store state (unknown → Idle).
    async fn status(&self) -> Result<Value> {
        let scenario = self.scenario.read().await;
        let rows = self.store.current_state().await?;
        let live = self.sup.snapshot().await;
        let services: Vec<Value> = scenario
            .services
            .iter()
            .map(|(name, spec)| {
                let row = rows.iter().find(|r| r.service == *name);
                let state = live
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, s)| *s)
                    .or(row.map(|r| r.state));
                json!({
                    "name": name,
                    "state": state.unwrap_or(ServiceState::Idle),
                    "port": spec.port,
                    "lazy": spec.lazy,
                    "provider": spec.provider,
                    "last_exit": row.and_then(|r| r.last_exit),
                })
            })
            .collect();
        Ok(json!({
            "name": scenario.scenario.name,
            "profile": rows.iter().find_map(|r| r.profile.clone()),
            "services": services,
            "jobs": scenario.jobs.keys().collect::<Vec<_>>(),
            "profiles": scenario.profiles.keys().collect::<Vec<_>>(),
        }))
    }

    /// Parse `EventQuery` filter (`k=v` pairs: `service=`, `kind=`, `tail=`).
    async fn event_query(&self, filter: Option<String>) -> Result<Value> {
        let mut service = None;
        let mut kind = None;
        let mut tail = 50u64;
        if let Some(f) = filter {
            for pair in f.split_whitespace() {
                let Some((k, v)) = pair.split_once('=') else {
                    continue;
                };
                match k {
                    "service" => service = Some(v.to_string()),
                    "kind" => kind = Some(v.to_string()),
                    "tail" => tail = v.parse().unwrap_or(tail),
                    _ => {}
                }
            }
        }
        let rows = self.store.event_query(service, kind, tail).await?;
        Ok(json!({ "events": rows }))
    }
}

fn io_err(e: std::io::Error) -> EngineError {
    EngineError::Ipc {
        message: e.to_string(),
    }
}

async fn bind_socket(socket_path: &Path) -> Result<UnixListener> {
    if let Some(dir) = socket_path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|source| EngineError::Socket {
                path: socket_path.to_path_buf(),
                source,
            })?;
    }
    if socket_path.exists() {
        // Possibly stale: only clear if no live daemon answers on it.
        match UnixStream::connect(socket_path).await {
            Ok(_) => {
                return Err(EngineError::Ipc {
                    message: format!(
                        "socket {} already answers — another daemon is running",
                        socket_path.display()
                    ),
                });
            }
            Err(_) => {
                tokio::fs::remove_file(socket_path)
                    .await
                    .map_err(|source| EngineError::Socket {
                        path: socket_path.to_path_buf(),
                        source,
                    })?;
            }
        }
    }
    UnixListener::bind(socket_path).map_err(|source| EngineError::Socket {
        path: socket_path.to_path_buf(),
        source,
    })
}

impl EngineError {
    fn into_reply(self) -> Reply {
        Reply::Err {
            message: self.to_string(),
        }
    }
}
