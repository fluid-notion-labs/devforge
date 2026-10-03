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
/// Order a profile's services by their advisory `after` edges restricted to
/// the set (companions are never pulled in). Config-order stable; cycles and
/// unknown deps degrade to config order.
pub fn after_order(
    set: &[String],
    services: &indexmap::IndexMap<String, ServiceSpec>,
) -> Vec<String> {
    let setv: std::collections::HashSet<&str> = set.iter().map(String::as_str).collect();
    let mut indeg: std::collections::HashMap<&str, usize> =
        set.iter().map(|n| (n.as_str(), 0)).collect();

    // adjacency: dep → dependents within the set
    let mut adj: std::collections::HashMap<&str, Vec<&str>> = Default::default();
    for name in set {
        let Some(spec) = services.get(name) else {
            continue;
        };
        for dep in &spec.after {
            if setv.contains(dep.as_str()) {
                *indeg.entry(name.as_str()).or_default() += 1;
                adj.entry(dep.as_str()).or_default().push(name.as_str());
            }
        }
    }

    // Kahn's algorithm, seeded in config order for stability.
    let mut head = std::collections::VecDeque::new();
    for name in set {
        if indeg.get(name.as_str()).copied().unwrap_or(0) == 0 {
            head.push_back(name.as_str());
        }
    }
    let mut out = Vec::with_capacity(set.len());
    while let Some(name) = head.pop_front() {
        out.push(name.to_string());
        let dependents = adj.get(name).cloned().unwrap_or_default();
        for dep in dependents {
            let e = indeg.get_mut(dep).unwrap();
            *e -= 1;
            if *e == 0 {
                head.push_back(dep);
            }
        }
    }
    // cycle leftovers in config order
    for name in set {
        if !out.iter().any(|o| o == name) {
            out.push(name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario() -> Scenario {
        let text = r#"
[scenario]
name = "t"

[services.api]
provider = "exec"
command = "x"

[services.web]
provider = "exec"
command = "x"
after = ["api"]

[services.search]
provider = "exec"
command = "x"
lazy = true
after = ["web"]

[services.loop1]
provider = "exec"
command = "x"
after = ["loop2"]

[services.loop2]
provider = "exec"
command = "x"
after = ["loop1"]

[profiles]
all = ["web", "api"]
"#;
        toml::from_str(text).unwrap()
    }

    #[test]
    fn after_edges_reorder_within_set() {
        let s = scenario();
        let set = vec!["web".to_string(), "api".to_string()];
        assert_eq!(after_order(&set, &s.services), vec!["api", "web"]);
    }

    #[test]
    fn default_profile_excludes_lazy() {
        let s = scenario();
        let set: Vec<String> = s
            .services
            .iter()
            .filter(|(_, sp)| !sp.lazy)
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(set, vec!["api", "web", "loop1", "loop2"]);
    }

    #[test]
    fn cycles_fall_back_to_config_order() {
        let s = scenario();
        let set = vec!["loop1".to_string(), "loop2".to_string()];
        assert_eq!(after_order(&set, &s.services), vec!["loop1", "loop2"]);
    }
}
