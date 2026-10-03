//! IPC contract: control verbs + state stream over the local socket.
//! Seat of truth is the daemon; TUI, GPUI, and MCP are all clients of these types.

use serde::{Deserialize, Serialize};

use crate::state::Transition;

/// Request/response control verbs (JSON over the socket).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "verb")]
pub enum Verb {
    ScenarioStart {
        profile: Option<String>,
    },
    ScenarioStop,
    ServiceStart {
        name: String,
        wait_for_ready: Option<bool>,
    },
    ServiceRestart {
        name: String,
    },
    ServiceStop {
        name: String,
    },
    JobRun {
        name: String,
    },
    ScenarioStatus,
    ServiceLogs {
        name: String,
        tail: Option<usize>,
    },
    EventQuery {
        filter: Option<String>,
    },
    ScenarioReload,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reply", content = "data")]
pub enum Reply {
    Ok(serde_json::Value),
    Err { message: String },
}

/// Push channel: every transition / build signal, fanned to all subscribers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum StreamEvent {
    Transition(Transition),
    BuildSignal { service: String, signal: String },
}

pub const DEFAULT_SOCKET_PATH: &str = ".devforge/socket";
