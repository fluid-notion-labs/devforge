use serde::{Deserialize, Serialize};

/// The shared service state machine — common vocabulary for TUI, GPUI, and MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Idle,
    Starting,
    Up,
    Compiling,
    Failed,
    Stopping,
}

/// Cause of a state transition, recorded to sqlite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cause")]
pub enum TransitionCause {
    UserRequest,
    ProviderPattern { pattern: String },
    Crash { exit_code: Option<i32> },
    Timeout,
    Watchdog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub service: String,
    pub from: ServiceState,
    pub to: ServiceState,
    #[serde(flatten)]
    pub cause: TransitionCause,
    pub at_unix_ms: u64,
}

impl ServiceState {
    /// UI dot mapping: grey, yellow, green, red, dim.
    pub fn dot(self) -> &'static str {
        match self {
            Self::Idle => "grey",
            Self::Starting | Self::Compiling => "yellow",
            Self::Up => "green",
            Self::Failed => "red",
            Self::Stopping => "dim",
        }
    }
}
