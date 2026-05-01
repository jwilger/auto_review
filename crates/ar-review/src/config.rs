//! Repo-level `.auto_review.yaml` configuration.
//!
//! Read once per review from the cloned workspace. Anything missing falls
//! back to defaults. Callers should treat the loader as best-effort: a
//! malformed YAML shouldn't break the review pipeline, just emit defaults
//! and log a warning.

use serde::{Deserialize, Serialize};
use std::path::Path;

const CONFIG_FILENAME: &str = ".auto_review.yaml";
const ALT_CONFIG_FILENAME: &str = ".auto_review.yml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Top-level switch. When false, the bot skips reviewing this repo
    /// entirely (still posts a "skipped (config)" status).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Repo-author free-form rules, injected into the LLM system prompt.
    /// Use this for project-specific conventions ("we never use raw
    /// SQL", "always prefer immutable types", etc.).
    #[serde(default)]
    pub guidelines: String,

    /// Path globs to skip reviewing. Gitignore-flavored — anything
    /// matching is filtered out of the changed-files list before linter
    /// routing and prompt rendering.
    #[serde(default)]
    pub ignored_paths: Vec<String>,

    /// Names of linters to disable (matching `LinterRunner::name()`).
    /// Useful for repos that have their own CI lint pipeline and don't
    /// want duplicate findings.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            guidelines: String::new(),
            ignored_paths: Vec::new(),
            disabled_tools: Vec::new(),
        }
    }
}

/// Load the repo-level config from a cloned workspace. Returns
/// `RepoConfig::default()` if no config file is present or parsing fails;
/// in the latter case, a warning is logged.
pub fn load_repo_config(workspace_path: &Path) -> RepoConfig {
    for name in [CONFIG_FILENAME, ALT_CONFIG_FILENAME] {
        let path = workspace_path.join(name);
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_yaml::from_str::<RepoConfig>(&contents) {
                Ok(cfg) => {
                    tracing::debug!(
                        path = %path.display(),
                        enabled = cfg.enabled,
                        ignored = cfg.ignored_paths.len(),
                        disabled_tools = cfg.disabled_tools.len(),
                        "loaded repo config"
                    );
                    return cfg;
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to parse repo config; using defaults");
                    return RepoConfig::default();
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read repo config; using defaults");
                return RepoConfig::default();
            }
        }
    }
    RepoConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn default_config_enables_review_with_no_overrides() {
        let cfg = RepoConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.guidelines.is_empty());
        assert!(cfg.ignored_paths.is_empty());
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn missing_config_file_returns_default() {
        let dir = tempdir().unwrap();
        let cfg = load_repo_config(dir.path());
        assert_eq!(cfg, RepoConfig::default());
    }

    #[test]
    fn parses_full_yaml_config() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".auto_review.yaml"),
            r#"
enabled: true
guidelines: |
  Always prefer total functions over partial ones.
  Forbid unwrap() outside tests.
ignored_paths:
  - "vendor/**"
  - "src/generated/**"
disabled_tools:
  - markdownlint
"#,
        )
        .unwrap();

        let cfg = load_repo_config(dir.path());
        assert!(cfg.enabled);
        assert!(cfg.guidelines.contains("total functions"));
        assert_eq!(cfg.ignored_paths.len(), 2);
        assert_eq!(cfg.disabled_tools, vec!["markdownlint"]);
    }

    #[test]
    fn enabled_false_disables_review() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".auto_review.yaml"), "enabled: false\n").unwrap();
        let cfg = load_repo_config(dir.path());
        assert!(!cfg.enabled);
    }

    #[test]
    fn yml_extension_is_also_recognized() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".auto_review.yml"), "enabled: false\n").unwrap();
        let cfg = load_repo_config(dir.path());
        assert!(!cfg.enabled);
    }

    #[test]
    fn yaml_takes_precedence_over_yml_when_both_present() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".auto_review.yaml"), "enabled: true\n").unwrap();
        fs::write(dir.path().join(".auto_review.yml"), "enabled: false\n").unwrap();
        let cfg = load_repo_config(dir.path());
        assert!(cfg.enabled);
    }

    #[test]
    fn malformed_yaml_falls_back_to_default() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".auto_review.yaml"),
            "enabled: not_a_bool\n",
        )
        .unwrap();
        let cfg = load_repo_config(dir.path());
        assert_eq!(cfg, RepoConfig::default());
    }

    #[test]
    fn partial_config_merges_with_defaults() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".auto_review.yaml"),
            "ignored_paths:\n  - vendor/**\n",
        )
        .unwrap();
        let cfg = load_repo_config(dir.path());
        // Unset fields keep their defaults.
        assert!(cfg.enabled);
        assert!(cfg.guidelines.is_empty());
        assert_eq!(cfg.ignored_paths, vec!["vendor/**"]);
    }
}
