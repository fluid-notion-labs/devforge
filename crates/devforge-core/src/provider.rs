//! Declarative providers: npm, wrangler, cargo, exec. The hydra cap stands —
//! anything else is `exec` + explicit patterns.

use std::path::{Path, PathBuf};

use crate::config::ServiceSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Npm,
    Wrangler,
    Cargo,
    Exec,
}

impl ProviderKind {
    pub fn from_spec(spec: &ServiceSpec) -> Self {
        match spec.provider.as_str() {
            "npm" => Self::Npm,
            "wrangler" => Self::Wrangler,
            "cargo" => Self::Cargo,
            _ => Self::Exec,
        }
    }

    /// argv to spawn for this service, given its package.json scripts (npm/wrangler).
    pub fn argv(&self, spec: &ServiceSpec, npm_script: Option<&String>) -> Option<Vec<String>> {
        match self {
            Self::Npm => spec
                .script
                .as_ref()
                .or(npm_script)
                .map(|s| vec!["npm".into(), "run".into(), s.clone()]),
            Self::Wrangler => spec
                .script
                .as_ref()
                .or(npm_script)
                .map(|s| vec!["npm".into(), "run".into(), s.clone()]),
            Self::Cargo | Self::Exec => spec
                .command
                .as_ref()
                .and_then(|cmd| shell_words::split(cmd).ok().filter(|v| !v.is_empty())),
        }
    }

    pub fn cwd(&self, spec: &ServiceSpec, root: &Path) -> PathBuf {
        spec.cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf())
    }
}
