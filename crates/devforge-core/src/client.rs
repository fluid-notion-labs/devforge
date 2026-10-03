//! IPC client: the one connection helper every front-end uses (TUI, GPUI, MCP
//! adapters). Newline-delimited JSON `Verb` in, `Reply` out; `subscribe`
//! opens a second connection and pushes `StreamEvent`s (plan/spec.md — one engine API).

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::error::{EngineError, Result};
use crate::ipc::{Reply, StreamEvent, Verb};

/// ScenarioStatus reply shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ScenarioStatus {
    pub name: String,
    pub profile: Option<String>,
    pub services: Vec<ServiceInfo>,
    #[serde(default)]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub state: crate::state::ServiceState,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub lazy: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub last_exit: Option<i32>,
}

/// ServiceLogs reply shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Logs {
    #[serde(default)]
    pub lines: Vec<crate::store::LogRow>,
}

pub struct Client {
    reader: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|e| EngineError::Ipc {
                message: format!("connect {}: {e}", socket.display()),
            })?;
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader).lines();
        Ok(Self { reader, writer })
    }

    /// Send one verb, read one reply.
    pub async fn call(&mut self, verb: &Verb) -> Reply {
        let mut line = match serde_json::to_vec(verb) {
            Ok(l) => l,
            Err(e) => {
                return Reply::Err {
                    message: e.to_string(),
                };
            }
        };
        line.push(b'\n');
        if let Err(e) = self.writer.write_all(&line).await {
            return Reply::Err {
                message: e.to_string(),
            };
        }
        match self.reader.next_line().await {
            Ok(Some(line)) => serde_json::from_str(&line).unwrap_or_else(|e| Reply::Err {
                message: e.to_string(),
            }),
            Ok(None) => Reply::Err {
                message: "socket closed".into(),
            },
            Err(e) => Reply::Err {
                message: e.to_string(),
            },
        }
    }

    /// Call ScenarioStatus and decode it; Err replies surface as engine errors.
    pub async fn status(&mut self) -> Result<ScenarioStatus> {
        match self.call(&Verb::ScenarioStatus).await {
            Reply::Ok(v) => Ok(serde_json::from_value(v)?),
            Reply::Err { message } => Err(EngineError::Ipc { message }),
        }
    }

    pub async fn logs(&mut self, name: &str, tail: usize) -> Result<Logs> {
        match self
            .call(&Verb::ServiceLogs {
                name: name.into(),
                tail: Some(tail),
            })
            .await
        {
            Reply::Ok(v) => Ok(serde_json::from_value(v)?),
            Reply::Err { message } => Err(EngineError::Ipc { message }),
        }
    }

    /// `package.json` scripts for an npm/wrangler service.
    pub async fn npm_scripts(&mut self, name: &str) -> Result<Vec<String>> {
        match self.call(&Verb::NpmScripts { name: name.into() }).await {
            Reply::Ok(v) => {
                let scripts = v.get("scripts").cloned().unwrap_or_default();
                Ok(serde_json::from_value(scripts)?)
            }
            Reply::Err { message } => Err(EngineError::Ipc { message }),
        }
    }

    /// Open a second connection, subscribe, and push every StreamEvent into
    /// `tx` (drop the receiver to stop the task).
    pub async fn subscribe(socket: PathBuf, tx: mpsc::UnboundedSender<StreamEvent>) -> Result<()> {
        let mut client = Self::connect(&socket).await?;
        let reply = client.call(&Verb::Subscribe {}).await;
        match reply {
            Reply::Ok(_) => {}
            Reply::Err { message } => return Err(EngineError::Ipc { message }),
        }
        loop {
            match client.reader.next_line().await {
                Ok(Some(line)) => match serde_json::from_str::<StreamEvent>(&line) {
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            return Ok(()); // receiver gone
                        }
                    }
                    Err(e) => {
                        return Err(EngineError::Ipc {
                            message: e.to_string(),
                        });
                    }
                },
                Ok(None) => return Ok(()),
                Err(e) => {
                    return Err(EngineError::Ipc {
                        message: e.to_string(),
                    });
                }
            }
        }
    }
}
