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
    /// matching is filtered out of the changed-files list before prompt
    /// rendering.
    #[serde(default)]
    pub ignored_paths: Vec<String>,

    #[serde(default, deserialize_with = "deserialize_pr_metadata_check")]
    pub pr_metadata_check: PrMetadataCheck,

    /// Forgejo logins permitted to force an override-approval over
    /// auto-review's outstanding findings. Opt-in: when empty (the default),
    /// no one may override — auto-review declines and explains how to
    /// configure this key. Matching is case-insensitive.
    #[serde(default)]
    pub override_approvers: Vec<String>,
}

impl RepoConfig {
    /// Whether `login` is authorized to force an override-approval over
    /// outstanding findings. Opt-in: an empty `override_approvers` list
    /// authorizes nobody. Matching is case-insensitive because Forgejo
    /// usernames are case-insensitive.
    pub fn is_override_approver(&self, login: &str) -> bool {
        self.override_approvers
            .iter()
            .any(|u| u.eq_ignore_ascii_case(login))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrMetadataCheck {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub checks: PrMetadataChecks,
    #[serde(default)]
    pub additional_rules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrMetadataChecks {
    #[serde(default = "default_true")]
    pub body_required: bool,
}

impl Default for PrMetadataCheck {
    fn default() -> Self {
        Self {
            enabled: true,
            checks: PrMetadataChecks::default(),
            additional_rules: Vec::new(),
        }
    }
}

impl Default for PrMetadataChecks {
    fn default() -> Self {
        Self {
            body_required: true,
        }
    }
}

fn deserialize_pr_metadata_check<'de, D>(deserializer: D) -> Result<PrMetadataCheck, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PrMetadataCheckConfig {
        Bool(bool),
        Object(PrMetadataCheck),
    }

    Ok(match PrMetadataCheckConfig::deserialize(deserializer)? {
        PrMetadataCheckConfig::Bool(value) => PrMetadataCheck {
            enabled: value,
            ..PrMetadataCheck::default()
        },
        PrMetadataCheckConfig::Object(value) => value,
    })
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
            pr_metadata_check: PrMetadataCheck::default(),
            override_approvers: Vec::new(),
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
    "pr_metadata_check",
    "override_approvers",
];

const KNOWN_PR_METADATA_CHECK_KEYS: &[&str] = &["enabled", "checks", "additional_rules"];
const KNOWN_PR_METADATA_CHECKS_KEYS: &[&str] = &["body_required"];

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

        if let Some(pr_metadata_check) =
            map.get(serde_yaml::Value::String("pr_metadata_check".into()))
        {
            if let Some(pr_metadata_check_map) = pr_metadata_check.as_mapping() {
                for (k, v) in pr_metadata_check_map {
                    if let Some(key_str) = k.as_str() {
                        if !KNOWN_PR_METADATA_CHECK_KEYS.contains(&key_str) {
                            unknown.push(format!("pr_metadata_check.{key_str}"));
                        }
                        if key_str == "checks" {
                            if let Some(checks_map) = v.as_mapping() {
                                for (check_key, _) in checks_map {
                                    if let Some(check_key_str) = check_key.as_str() {
                                        if !KNOWN_PR_METADATA_CHECKS_KEYS.contains(&check_key_str) {
                                            unknown.push(format!(
                                                "pr_metadata_check.checks.{check_key_str}"
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
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

/// Hard cap on the bytes we'll read from a repo-supplied
/// `.auto_review.yaml`. The file lives in the PR-cloned
/// workspace, so an attacker submitting a PR controls its
/// content. Without a cap, a malicious 1 GiB YAML would OOM the
/// gateway during load. 64 KiB easily holds any real config (the
/// example file in the repo is well under 1 KiB) — beyond that is
/// almost certainly an attack or a paste mistake.
const REPO_CONFIG_MAX_BYTES: u64 = 64 * 1024;

/// Load the repo-level config from a cloned workspace. Returns
/// `RepoConfig::default()` if no config file is present or parsing fails;
/// in the latter case, a warning is logged.
pub fn load_repo_config(workspace_path: &Path) -> RepoConfig {
    for name in [CONFIG_FILENAME, ALT_CONFIG_FILENAME] {
        let path = workspace_path.join(name);
        // Refuse to read pathologically large YAML files. A PR
        // can commit anything; without this, the read_to_string
        // call below would happily slurp gigabytes into RAM.
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > REPO_CONFIG_MAX_BYTES {
                tracing::warn!(
                    path = %path.display(),
                    bytes = meta.len(),
                    cap = REPO_CONFIG_MAX_BYTES,
                    "repo config exceeds size cap; using defaults"
                );
                return RepoConfig::default();
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_yaml::from_str::<RepoConfig>(&contents) {
                Ok(cfg) => {
                    tracing::debug!(
                        path = %path.display(),
                        enabled = cfg.enabled,
                        ignored = cfg.ignored_paths.len(),
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
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|m| m.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(true));
    }

    #[test]
    fn parses_pr_metadata_check_false() {
        let cfg = parse_repo_config("pr_metadata_check: false\n").expect("parse config");
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|m| m.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
    }

    #[test]
    fn parses_object_pr_metadata_check_enabled_false() {
        let cfg =
            parse_repo_config("pr_metadata_check:\n  enabled: false\n").expect("parse config");
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|m| m.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
    }

    #[test]
    fn parses_override_approvers_list() {
        let cfg = parse_repo_config("override_approvers:\n  - jwilger\n  - alice\n")
            .expect("parse config");
        assert_eq!(cfg.override_approvers, vec!["jwilger", "alice"]);
    }

    #[test]
    fn default_override_approvers_is_empty_opt_in() {
        let cfg = RepoConfig::default();
        assert!(cfg.override_approvers.is_empty());
        // Opt-in: nobody is authorized when the list is empty.
        assert!(!cfg.is_override_approver("jwilger"));
    }

    #[test]
    fn override_approver_match_is_case_insensitive() {
        let cfg = parse_repo_config("override_approvers:\n  - JWilger\n").expect("parse config");
        assert!(cfg.is_override_approver("jwilger"));
        assert!(cfg.is_override_approver("JWILGER"));
        assert!(!cfg.is_override_approver("someone-else"));
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
"#,
        )
        .unwrap();

        let cfg = load_repo_config(dir.path());
        assert!(cfg.enabled);
        assert!(cfg.guidelines.contains("total functions"));
        assert_eq!(cfg.ignored_paths.len(), 2);
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
        let serialised: std::collections::BTreeSet<&str> =
            map.iter().filter_map(|(k, _)| k.as_str()).collect();
        let allowed: std::collections::BTreeSet<&str> = KNOWN_KEYS.iter().copied().collect();
        assert_eq!(
            serialised, allowed,
            "RepoConfig fields and KNOWN_KEYS allow-list have drifted"
        );
    }

    #[test]
    fn strict_metadata_allowlists_match_struct_fields() {
        let pr_metadata_check = PrMetadataCheck::default();
        let value = serde_yaml::to_value(&pr_metadata_check).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised_check_keys: std::collections::BTreeSet<&str> =
            map.iter().filter_map(|(k, _)| k.as_str()).collect();
        let allowed_check_keys: std::collections::BTreeSet<&str> =
            KNOWN_PR_METADATA_CHECK_KEYS.iter().copied().collect();
        assert_eq!(
            serialised_check_keys, allowed_check_keys,
            "PrMetadataCheck fields and KNOWN_PR_METADATA_CHECK_KEYS allow-list have drifted"
        );

        let pr_metadata_checks = PrMetadataChecks::default();
        let value = serde_yaml::to_value(&pr_metadata_checks).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised_checks_keys: std::collections::BTreeSet<&str> =
            map.iter().filter_map(|(k, _)| k.as_str()).collect();
        let allowed_checks_keys: std::collections::BTreeSet<&str> =
            KNOWN_PR_METADATA_CHECKS_KEYS.iter().copied().collect();
        assert_eq!(
            serialised_checks_keys, allowed_checks_keys,
            "PrMetadataChecks fields and KNOWN_PR_METADATA_CHECKS_KEYS allow-list have drifted"
        );
    }

    #[test]
    fn strict_parses_known_config_cleanly() {
        let yaml = "enabled: true\npr_metadata_check: false\nignored_paths:\n  - vendor/**\n";
        let cfg = parse_repo_config_strict(yaml).expect("ok");
        assert!(cfg.enabled);
        assert_eq!(cfg.ignored_paths, vec!["vendor/**"]);
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|m| m.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
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
    fn strict_rejects_retired_linter_keys() {
        let yaml = "mode: linter_only\ndisabled_tools:\n  - ruff\npre_merge_checks:\n  - old\n";
        let err = parse_repo_config_strict(yaml).expect_err("retired keys should fail");
        let msg = format!("{err}");
        assert!(msg.contains("disabled_tools"), "{msg}");
        assert!(msg.contains("mode"), "{msg}");
        assert!(msg.contains("pre_merge_checks"), "{msg}");
    }

    #[test]
    fn strict_propagates_value_level_errors_through_serde() {
        let yaml = "enabled: not_a_bool\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        assert!(matches!(err, RepoConfigStrictError::Parse(_)));
    }

    #[test]
    fn strict_rejects_unknown_pr_metadata_check_keys() {
        let yaml =
            "pr_metadata_check:\n  checks:\n    body_requred: true\n    unsupported_nested: true\n";

        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        match err {
            RepoConfigStrictError::UnknownKeys(keys) => {
                assert!(keys.iter().any(|k| k.contains("pr_metadata_check.checks")))
            }
            RepoConfigStrictError::Parse(parse_err) => {
                let msg = parse_err.to_string();
                assert!(
                    msg.contains("body_requred") || msg.contains("unsupported_nested"),
                    "{msg}"
                );
            }
        }
    }

    #[test]
    fn oversized_config_file_falls_back_to_defaults_without_reading() {
        // A malicious PR could commit a multi-MB .auto_review.yaml
        // and OOM the gateway during load. The size-cap check
        // bypasses the read entirely on oversized files.
        let dir = tempdir().unwrap();
        // 200 KiB > 64 KiB cap. Content is irrelevant — the loader
        // shouldn't even parse it.
        let huge: String = "ignored_paths:\n  - foo\n".repeat(20_000);
        fs::write(dir.path().join(".auto_review.yaml"), &huge).unwrap();
        let cfg = load_repo_config(dir.path());
        // Default config — the oversize triggered the bypass, so
        // `ignored_paths` came from RepoConfig::default() (empty).
        assert!(cfg.ignored_paths.is_empty());
        assert!(cfg.enabled);
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
