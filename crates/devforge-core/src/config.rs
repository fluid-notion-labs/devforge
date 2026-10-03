use std::path::Path;

use serde::Deserialize;

use crate::error::{EngineError, Result};

/// Parsed `dev-scenario.toml` / `.devforge/scenario.toml`.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub scenario: ScenarioMeta,
    #[serde(default)]
    pub services: indexmap::IndexMap<String, ServiceSpec>,
    #[serde(default)]
    pub jobs: indexmap::IndexMap<String, JobSpec>,
    #[serde(default)]
    pub profiles: indexmap::IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioMeta {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceSpec {
    pub provider: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// `npm` provider: the package.json script name.
    #[serde(default)]
    pub script: Option<String>,
    /// `exec`/`cargo` providers: argv to spawn.
    #[serde(default)]
    pub command: Option<String>,
    /// Provider-known readiness pattern (e.g. "vite", "wrangler").
    #[serde(default)]
    pub ready_when: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub lazy: bool,
    #[serde(default)]
    pub after: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobSpec {
    #[serde(default)]
    pub cwd: Option<String>,
    pub command: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub socket: Option<String>,
}

fn default_mcp_enabled() -> bool {
    true
}

/// Load the scenario TOML from the repo root ( `.devforge/scenario.toml` then `dev-scenario.toml`).
pub fn load(root: &Path) -> Result<Scenario> {
    for rel in [".devforge/scenario.toml", "dev-scenario.toml"] {
        let path = root.join(rel);
        if path.is_file() {
            let text =
                std::fs::read_to_string(&path).map_err(|source| EngineError::ScenarioRead {
                    path: path.clone(),
                    source,
                })?;
            return toml::from_str(&text)
                .map_err(|source| EngineError::ScenarioParse { path, source });
        }
    }
    Ok(Scenario {
        scenario: ScenarioMeta {
            name: root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        },
        services: Default::default(),
        jobs: Default::default(),
        profiles: Default::default(),
        mcp: Default::default(),
    })
}
