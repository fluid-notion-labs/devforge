//! SQLite store: single connection owned by a `tokio-rusqlite` actor.
//! Runs/events/state per plan/spec.md "State & history (sqlite)".
//! Clients never touch this file — everything goes through the engine over IPC.

use std::path::Path;

use rusqlite::params;
use rusqlite_migration::{M, Migrations};
use tokio_rusqlite::Connection;
use tracing::info;

use crate::error::{EngineError, Result};
use crate::state::{ServiceState, Transition, TransitionCause};

/// Schema history. Every change appends a migration; never edit an applied one.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            "CREATE TABLE runs (
                id INTEGER PRIMARY KEY,
                service TEXT NOT NULL,
                profile TEXT,
                pid INTEGER,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                exit_code INTEGER,
                cwd TEXT,
                command TEXT
            );
            CREATE INDEX idx_runs_service ON runs(service, started_at);",
        ),
        M::up(
            "CREATE TABLE events (
                id INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                service TEXT,
                kind TEXT NOT NULL,
                payload TEXT
            );
            CREATE INDEX idx_events_service_kind ON events(service, kind, at);",
        ),
        M::up(
            "CREATE TABLE state (
                service TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                port INTEGER,
                last_exit INTEGER,
                profile TEXT,
                updated_at INTEGER NOT NULL
            );",
        ),
    ])
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating) the database at `path`, applying pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|source| EngineError::StoreOpen {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let path = path.to_path_buf();
        let conn = Connection::open(path.clone())
            .await
            .map_err(|e| EngineError::StoreOpenIo {
                path: path.clone(),
                message: e.to_string(),
            })?;
        conn.call(|conn| -> rusqlite::Result<()> {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "busy_timeout", 5000)?;
            Ok(())
        })
        .await
        .map_err(|e| EngineError::StoreOpenIo {
            path: path.clone(),
            message: e.to_string(),
        })?;
        conn.call(
            |conn| -> std::result::Result<(), rusqlite_migration::Error> {
                migrations().to_latest(conn)
            },
        )
        .await
        .map_err(|e| EngineError::StoreOpenIo {
            path: path.clone(),
            message: e.to_string(),
        })?;
        info!(path = %path.display(), "store opened (WAL, migrations current)");
        Ok(Self { conn })
    }

    pub async fn close(self) {
        let _ = self.conn.close().await;
    }

    /// Record a state transition: one `events` row + upsert into `state`.
    pub async fn record_transition(&self, t: &Transition) -> Result<()> {
        let payload = serde_json::to_string(&t.cause)
            .expect("TransitionCause serializes; serde derive cannot fail");
        let service = t.service.clone();
        let serde_json_str = serde_json::to_string(&t.to)
            .map_err(EngineError::Json)
            .unwrap_or_else(|_| format!("{:?}", t.to).to_lowercase());
        let at = t.at_unix_ms as i64;
        self.conn
            .call(move |conn| -> rusqlite::Result<_> {
                conn.execute(
                    "INSERT INTO events (at, service, kind, payload)
                     VALUES (?1, ?2, 'state_transition', ?3)",
                    params![at, service, payload],
                )?;
                conn.execute(
                    "INSERT INTO state (service, state, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(service) DO UPDATE
                     SET state = excluded.state, updated_at = excluded.updated_at",
                    params![service, serde_json_str, at],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| EngineError::StoreQuery {
                message: e.to_string(),
            })?;
        Ok(())
    }

    /// Current engine view: all rows of `state`.
    pub async fn current_state(&self) -> Result<Vec<ServiceStateRow>> {
        self.conn
            .call(move |conn| -> rusqlite::Result<_> {
                let mut stmt = conn.prepare(
                    "SELECT service, state, port, last_exit, profile, updated_at FROM state",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(ServiceStateRow {
                            service: r.get(0)?,
                            state: serde_json::from_str(&r.get::<_, String>(1)?)
                                .unwrap_or(ServiceState::Idle),
                            port: r.get(2)?,
                            last_exit: r.get(3)?,
                            profile: r.get(4)?,
                            updated_at: r.get::<_, i64>(5)? as u64,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| EngineError::StoreQuery {
                message: e.to_string(),
            })
    }

    /// Log tail for a service: last `tail` log_line rows, oldest last.
    pub async fn service_logs(&self, service: String, tail: usize) -> Result<Vec<LogRow>> {
        self.conn
            .call(move |conn| -> rusqlite::Result<_> {
                let mut stmt = conn.prepare(
                    "SELECT at, payload FROM events
                     WHERE service = ?1 AND kind = 'log_line'
                     ORDER BY at DESC, id DESC LIMIT ?2",
                )?;
                let mut rows = stmt
                    .query_map(params![service, tail as i64], |r| {
                        Ok(LogRow {
                            at: r.get::<_, i64>(0)? as u64,
                            line: r.get(1)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows.reverse();
                Ok(rows)
            })
            .await
            .map_err(|e| EngineError::StoreQuery {
                message: e.to_string(),
            })
    }

    /// Append a log line event (falls under the `events` table, kind = log_line).
    pub async fn append_log(&self, service: String, line: String, at_unix_ms: u64) -> Result<()> {
        self.conn
            .call(move |conn| -> rusqlite::Result<_> {
                conn.execute(
                    "INSERT INTO events (at, service, kind, payload) VALUES (?1, ?2, 'log_line', ?3)",
                    params![at_unix_ms as i64, service, line],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| EngineError::StoreQuery { message: e.to_string() })?;
        Ok(())
    }

    /// Last `tail` event rows for an `event_query`, newest last; optional
    /// `service`/`kind` filters.
    pub async fn event_query(
        &self,
        service: Option<String>,
        kind: Option<String>,
        tail: u64,
    ) -> Result<Vec<EventRow>> {
        self.conn
            .call(move |conn| -> rusqlite::Result<_> {
                let mut sql = "SELECT at, service, kind, payload FROM events".to_string();
                let mut clauses = Vec::new();
                if service.is_some() {
                    clauses.push("service = ?1");
                }
                if kind.is_some() {
                    clauses.push("kind = ?2");
                }
                if !clauses.is_empty() {
                    sql.push_str(" WHERE ");
                    sql.push_str(&clauses.join(" AND "));
                }
                sql.push_str(" ORDER BY at DESC, id DESC LIMIT ?3");
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt
                    .query_map(params![service, kind, tail as i64], |r| {
                        Ok(EventRow {
                            at: r.get::<_, i64>(0)? as u64,
                            service: r.get(1)?,
                            kind: r.get(2)?,
                            payload: r.get(3)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows.reverse();
                Ok(rows)
            })
            .await
            .map_err(|e| EngineError::StoreQuery {
                message: e.to_string(),
            })
    }
}

/// Row of the `state` table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceStateRow {
    pub service: String,
    pub state: ServiceState,
    pub port: Option<u16>,
    pub last_exit: Option<i32>,
    pub profile: Option<String>,
    pub updated_at: u64,
}

/// Row of a `log_line` event.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogRow {
    pub at: u64,
    pub line: String,
}

/// Row of an `events` query result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EventRow {
    pub at: u64,
    pub service: Option<String>,
    pub kind: String,
    pub payload: Option<String>,
}

#[allow(unused)]
fn unused(cause: &TransitionCause) {}
