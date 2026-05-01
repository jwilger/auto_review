//! Repo-level `.auto_review.yaml` configuration.
//!
//! Read once per review from the cloned workspace. Anything missing falls
//! back to defaults. Callers should treat the loader as best-effort: a
//! malformed YAML shouldn't break the review pipeline, just emit defaults
//! and log a warning.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    /// LLM-driven review with linter findings as supplementary
    /// context. The default; what every existing deployment runs.
    #[default]
    Full,
    /// Skip the LLM review entirely. The bot still clones the
    /// workspace and runs the linter pipeline; findings are mapped
    /// straight to inline review comments. No LLM tokens are spent.
    /// Useful for repos that want centralized linter aggregation
    /// without the cost or latency of LLM review, or as a stepping
    /// stone before opting into the full review.
    LinterOnly,
}

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

    /// Review behaviour. `Full` (default) runs the full LLM
    /// review with linter context. `LinterOnly` skips the LLM
    /// entirely and posts linter findings as inline comments —
    /// zero token cost but no semantic review.
    #[serde(default)]
    pub mode: ReviewMode,

    /// Repo-author-supplied natural-language pre-merge checks. Each
    /// entry is evaluated against the diff by the cheap LLM tier and
    /// surfaces in the review body's "Pre-merge checks" section
    /// alongside the built-in deterministic checks.
    /// Example:
    ///   pre_merge_checks:
    ///     - "All new public APIs have rustdoc comments"
    ///     - "No raw SQL queries; everything goes through QueryBuilder"
    /// Skipped silently when the cheap tier is unconfigured.
    #[serde(default)]
    pub pre_merge_checks: Vec<String>,
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
            mode: ReviewMode::Full,
            pre_merge_checks: Vec::new(),
        }
    }
}

/// Parse a `.auto_review.yaml` body into a [`RepoConfig`], surfacing
/// errors verbatim. Used by the `auto_review validate-config` CLI
/// subcommand; the runtime loader [`load_repo_config`] swallows the
/// same errors and falls back to defaults so a malformed config
/// can't break the review pipeline.
pub fn parse_repo_config(yaml: &str) -> Result<RepoConfig, serde_yaml::Error> {
    serde_yaml::from_str::<RepoConfig>(yaml)
}

/// Allow-list of every top-level key the loader recognizes. Kept
/// in sync with [`RepoConfig`] manually — the contract test
/// `strict_allowlist_matches_struct_fields` in `config.rs` pins
/// the relationship.
const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "guidelines",
    "ignored_paths",
    "disabled_tools",
    "mode",
    "pre_merge_checks",
];

/// Strict parser: surfaces unknown top-level keys as errors so a
/// typo like `enabld: true` (missing `e`) is caught at validation
/// time instead of silently parsing as default values.
///
/// Layered over [`parse_repo_config`] rather than replacing it
/// because the runtime loader is intentionally permissive — a
/// repo that's pinned to an older auto_review version shouldn't
/// break when someone adds a forward-compat field. The strict
/// check is opt-in via `auto_review validate-config --strict`.
pub fn parse_repo_config_strict(yaml: &str) -> Result<RepoConfig, RepoConfigStrictError> {
    // First: parse generically and inspect top-level keys.
    let raw: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(RepoConfigStrictError::Parse)?;
    if let Some(map) = raw.as_mapping() {
        let mut unknown: Vec<String> = Vec::new();
        for (k, _) in map {
            if let Some(key_str) = k.as_str() {
                if !KNOWN_KEYS.contains(&key_str) {
                    unknown.push(key_str.to_string());
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            return Err(RepoConfigStrictError::UnknownKeys(unknown));
        }
    }
    // Then: defer to the permissive parser for value-level errors.
    parse_repo_config(yaml).map_err(RepoConfigStrictError::Parse)
}

#[derive(Debug, thiserror::Error)]
pub enum RepoConfigStrictError {
    #[error("yaml: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("unknown top-level key(s): {}; valid keys are: {}",
        .0.join(", "),
        KNOWN_KEYS.join(", "))]
    UnknownKeys(Vec<String>),
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

    /// Cross-file contract test: every key in `KNOWN_KEYS` must
    /// appear in `.auto_review.example.yaml` (commented or
    /// uncommented). Adding a config field without documenting
    /// it in the example means downstream operators discover it
    /// only by reading source. The test catches that drift at
    /// CI time.
    #[test]
    fn example_yaml_documents_every_known_key() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let example_path = manifest.join(".auto_review.example.yaml");
        let body = std::fs::read_to_string(&example_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", example_path.display()));
        let mut missing = Vec::new();
        for key in KNOWN_KEYS {
            // Match either `key:` (uncommented) or `# key:` (commented
            // example) — at line start, optionally with leading
            // whitespace. We just check substring membership; the
            // YAML parser already validates structure.
            let needle = format!("{key}:");
            if !body.contains(&needle) {
                missing.push(*key);
            }
        }
        assert!(
            missing.is_empty(),
            "config keys missing from .auto_review.example.yaml: {missing:?}"
        );
    }

    /// Contract test: the `KNOWN_KEYS` allow-list must match the
    /// fields on `RepoConfig` exactly. Adding a field to the
    /// struct without updating `KNOWN_KEYS` would make
    /// `parse_repo_config_strict` reject legitimate configs.
    #[test]
    fn strict_allowlist_matches_struct_fields() {
        // Round-trip a default config through serde_yaml as JSON
        // (which exposes field names) and confirm every key is in
        // the allow-list.
        let cfg = RepoConfig::default();
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised: std::collections::BTreeSet<&str> = map
            .iter()
            .filter_map(|(k, _)| k.as_str())
            .collect();
        let allowed: std::collections::BTreeSet<&str> = KNOWN_KEYS.iter().copied().collect();
        assert_eq!(
            serialised, allowed,
            "RepoConfig fields and KNOWN_KEYS allow-list have drifted"
        );
    }

    #[test]
    fn strict_parses_known_config_cleanly() {
        let yaml = "enabled: true\nignored_paths:\n  - vendor/**\n";
        let cfg = parse_repo_config_strict(yaml).expect("ok");
        assert!(cfg.enabled);
        assert_eq!(cfg.ignored_paths, vec!["vendor/**"]);
    }

    #[test]
    fn strict_rejects_typo_in_top_level_key() {
        // Missing 'e' in 'enabled'.
        let yaml = "enabld: true\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        let msg = format!("{err}");
        assert!(msg.contains("enabld"), "{msg}");
        assert!(msg.contains("valid keys"), "{msg}");
    }

    #[test]
    fn strict_lists_multiple_unknown_keys_alphabetically() {
        let yaml = "ignord: x\nbogus: 1\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        let msg = format!("{err}");
        // Sorted alphabetically.
        let bogus_pos = msg.find("bogus").expect("bogus");
        let ignord_pos = msg.find("ignord").expect("ignord");
        assert!(bogus_pos < ignord_pos, "{msg}");
    }

    #[test]
    fn strict_propagates_value_level_errors_through_serde() {
        let yaml = "enabled: not_a_bool\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        assert!(matches!(err, RepoConfigStrictError::Parse(_)));
    }

    #[test]
    fn mode_defaults_to_full() {
        let cfg = RepoConfig::default();
        assert_eq!(cfg.mode, ReviewMode::Full);
    }

    #[test]
    fn mode_linter_only_parses() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".auto_review.yaml"), "mode: linter_only\n").unwrap();
        let cfg = load_repo_config(dir.path());
        assert_eq!(cfg.mode, ReviewMode::LinterOnly);
    }

    #[test]
    fn mode_invalid_value_falls_back_to_default_via_loader() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".auto_review.yaml"), "mode: bogus\n").unwrap();
        let cfg = load_repo_config(dir.path());
        // Loader falls back to RepoConfig::default() on parse error;
        // RepoConfig::default().mode is Full.
        assert_eq!(cfg.mode, ReviewMode::Full);
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
