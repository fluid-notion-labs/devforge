//! Engine daemon: IPC server binding the local socket, dispatching verbs and
//! fanning `StreamEvent`s. Seat of truth; front-ends connect over the socket (plan/spec.md).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{self, Scenario};
use crate::error::{EngineError, Result};
use crate::ipc::{DEFAULT_SOCKET_PATH, Reply, Verb};
use crate::state::ServiceState;
use crate::store::Store;

/// The engine: scenario config + sqlite store, shared by all IPC connections.
pub struct Engine {
    pub root: PathBuf,
    scenario: RwLock<Scenario>,
    store: Store,
}

impl Engine {
    /// Open the store under `root/.devforge/` and load the scenario.
    pub async fn open(root: PathBuf) -> Result<Self> {
        let scenario = config::load(&root)?;
        let store = Store::open(&root.join(".devforge/state.sqlite3")).await?;
        info!(root = %root.display(), name = %scenario.scenario.name, "engine open");
        Ok(Self {
            root,
            scenario: RwLock::new(scenario),
            store,
        })
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
        while let Some(line) = lines.next_line().await.map_err(|e| EngineError::Ipc {
            message: e.to_string(),
        })? {
            let reply = match serde_json::from_str::<Verb>(&line) {
                Ok(verb) => self.dispatch(verb).await,
                Err(e) => Reply::Err {
                    message: format!("bad verb: {e}"),
                },
            };
            let mut out = serde_json::to_vec(&reply).map_err(EngineError::from)?;
            out.push(b'\n');
            writer.write_all(&out).await.map_err(|e| EngineError::Ipc {
                message: e.to_string(),
            })?;
        }
        Ok(())
    }

    /// One verb → one reply.
    pub async fn dispatch(&self, verb: Verb) -> Reply {
        match verb {
            Verb::ScenarioStatus => match self.status().await {
                Ok(v) => Reply::Ok(v),
                Err(e) => e.into_reply(),
            },
            Verb::ScenarioStart { profile } => {
                match (profile, self.scenario.read().await.profiles.clone()) {
                    (Some(p), profiles) if !profiles.contains_key(&p) => {
                        EngineError::UnknownProfile { name: p }.into_reply()
                    }
                    _ => Reply::Err {
                        message: "process supervision not implemented yet".into(),
                    },
                }
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
            Verb::ScenarioStop
            | Verb::ServiceStart { .. }
            | Verb::ServiceRestart { .. }
            | Verb::ServiceStop { .. }
            | Verb::JobRun { .. } => Reply::Err {
                message: "process supervision not implemented yet".into(),
            },
        }
    }

    async fn known_service(&self, name: &str) -> bool {
        self.scenario.read().await.services.contains_key(name)
    }

    /// ScenarioStatus payload: config merged with store state (unknown → Idle).
    async fn status(&self) -> Result<Value> {
        let scenario = self.scenario.read().await;
        let rows = self.store.current_state().await?;
        let services: Vec<Value> = scenario
            .services
            .iter()
            .map(|(name, spec)| {
                let row = rows.iter().find(|r| r.service == *name);
                json!({
                    "name": name,
                    "state": row.map_or(ServiceState::Idle, |r| r.state),
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
