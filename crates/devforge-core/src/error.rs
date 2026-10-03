use std::path::PathBuf;

use thiserror::Error;

/// Top-level error type for the engine; wrapped into `anyhow::Error` at the bin boundary.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("failed to read scenario at {}: {source}", path.display())]
    ScenarioRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse scenario at {}: {source}", path.display())]
    ScenarioParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("unknown service: {name}")]
    UnknownService { name: String },
    #[error("unknown job: {name}")]
    UnknownJob { name: String },
    #[error("profile `{name}` is not defined")]
    UnknownProfile { name: String },
    #[error("failed to create store dir {}: {source}", path.display())]
    StoreOpen {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open store at {}: {message}", path.display())]
    StoreOpenIo { path: PathBuf, message: String },
    #[error("store query failed: {message}")]
    StoreQuery { message: String },
    #[error("failed to bind socket {}: {source}", path.display())]
    Socket {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ipc error: {message}")]
    Ipc { message: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = EngineError> = std::result::Result<T, E>;
