//! Repo-level `.auto_review.yaml` configuration.
//!
//! This module only parses config bodies. Runtime loading goes through
//! [`crate::trusted_files::load_repo_config_from_base`], which reads the
//! file from the pull request's base branch, never from the head checkout,
//! so a pull request cannot change how it is reviewed. Anything missing
//! falls back to defaults.

use alloc::collections::BTreeMap;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};

/// Allow-list of every top-level key the loader recognizes. Kept
/// in sync with [`RepoConfig`] manually — the contract test
/// `strict_allowlist_matches_struct_fields` in `config.rs` pins
/// the relationship.
const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "github_automatic_review",
    "guidelines",
    "ignored_paths",
    "pr_metadata_check",
    "override_approvers",
];

/// Allow-list of the keys inside a `pr_metadata_check` mapping.
const KNOWN_PR_METADATA_CHECK_KEYS: &[&str] = &["enabled", "checks", "additional_rules"];
/// Allow-list of the keys inside `pr_metadata_check.checks`.
const KNOWN_PR_METADATA_CHECKS_KEYS: &[&str] = &["body_required"];

/// Parsed `.auto_review.yaml` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-orchestrator (dispatcher) and ar-chat (handler) import this as \
              ar_review::RepoConfig; renaming it breaks those crates"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized YAML key order and matches the documented key order \
              in KNOWN_KEYS; reordering changes serialized config output"
)]
#[expect(
    clippy::partial_pub_fields,
    reason = "the deferred GitHub policy must be decoded through its validating accessor"
)]
pub struct RepoConfig {
    /// Top-level switch. When false, the bot skips reviewing this repo
    /// entirely (still posts a "skipped (config)" status).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Admission policy, decoded separately so a GitHub-only error does not discard shared settings.
    #[serde(default)]
    github_automatic_review: serde_yaml::Value,

    /// Repo-author free-form project conventions ("we never use raw
    /// SQL", "always prefer immutable types", etc.). They reach the
    /// review prompt inside a `guidelines` untrusted-data fence, so they
    /// shape what the reviewer flags but cannot override the system
    /// prompt's rules, severity definitions or output contract.
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
    /// Decode the automatic GitHub App review policy.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid policy values or unusable check names.
    #[inline]
    pub fn github_automatic_review(&self) -> Result<GitHubAutomaticReview, serde_yaml::Error> {
        if self.github_automatic_review.is_null() {
            return Ok(GitHubAutomaticReview::default());
        }
        let policy: GitHubAutomaticReview =
            serde_yaml::from_value(self.github_automatic_review.clone())?;
        if policy
            .required_checks
            .keys()
            .any(|name| name.trim().is_empty() || name.eq_ignore_ascii_case("auto_review"))
        {
            return Err(serde_yaml::Error::custom("github_automatic_review.required_checks cannot contain a blank name or the reserved auto_review context"));
        }
        Ok(policy)
    }

    /// Whether `login` is authorized to force an override-approval over
    /// outstanding findings. Opt-in: an empty `override_approvers` list
    /// authorizes nobody. Matching is case-insensitive because Forgejo
    /// usernames are case-insensitive.
    #[inline]
    #[must_use = "the check has no side effects; callers act only on its answer"]
    pub fn is_override_approver(&self, login: &str) -> bool {
        self.override_approvers
            .iter()
            .any(|approver| approver.eq_ignore_ascii_case(login))
    }
}

impl Default for RepoConfig {
    #[inline]
    fn default() -> Self {
        Self {
            enabled: true,
            github_automatic_review: serde_yaml::Value::Null,
            guidelines: String::new(),
            ignored_paths: Vec::new(),
            pr_metadata_check: PrMetadataCheck::default(),
            override_approvers: Vec::new(),
        }
    }
}

/// Repository policy for automatic GitHub App reviews.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct GitHubAutomaticReview {
    /// Also review open draft PRs.
    pub allow_drafts: bool,
    /// Check-run names or commit status contexts that must be satisfied.
    pub required_checks: BTreeMap<String, CheckRequirement>,
}

/// Minimum result a named CI job must reach before automatic review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckRequirement {
    /// Any completed outcome, including failure.
    Complete,
    /// Success, skipped or neutral.
    Pass,
}

/// `pr_metadata_check` section: whether and how PR title/body are checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "the red-team tests build PrMetadataCheck with a struct literal; \
              #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized YAML key order and matches \
              KNOWN_PR_METADATA_CHECK_KEYS; reordering changes serialized config output"
)]
pub struct PrMetadataCheck {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub checks: PrMetadataChecks,
    #[serde(default)]
    pub additional_rules: Vec<String>,
}

/// Individual built-in PR metadata checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PrMetadataChecks {
    #[serde(default = "default_true")]
    pub body_required: bool,
}

impl Default for PrMetadataCheck {
    #[inline]
    fn default() -> Self {
        Self {
            enabled: true,
            checks: PrMetadataChecks::default(),
            additional_rules: Vec::new(),
        }
    }
}

impl Default for PrMetadataChecks {
    #[inline]
    fn default() -> Self {
        Self {
            body_required: true,
        }
    }
}

/// Why [`parse_repo_config_strict`] rejected a config body.
#[derive(Debug, thiserror::Error)]
pub enum RepoConfigStrictError {
    #[error("yaml: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("unknown top-level key(s): {}; valid keys are: {}",
        .0.join(", "),
        KNOWN_KEYS.join(", "))]
    UnknownKeys(Vec<String>),
}

/// Accept `pr_metadata_check` as either a bare boolean (the `enabled`
/// switch, other fields defaulted) or a full mapping.
fn deserialize_pr_metadata_check<'de, D>(deserializer: D) -> Result<PrMetadataCheck, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// The two accepted YAML shapes of `pr_metadata_check`.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PrMetadataCheckConfig {
        /// Bare `true` / `false`.
        Bool(bool),
        /// Full mapping.
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

/// Serde default for switches that are on unless configured off.
fn default_true() -> bool {
    true
}

/// Parse a `.auto_review.yaml` body into a [`RepoConfig`], surfacing
/// errors verbatim. Used by the `auto_review validate-config` CLI
/// subcommand; the runtime loader
/// [`crate::trusted_files::load_repo_config_from_base`] swallows the
/// same errors and falls back to defaults so a malformed config
/// can't break the review pipeline.
///
/// # Errors
///
/// Returns the YAML error when the body is not valid YAML or does not
/// match the [`RepoConfig`] shape.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-cli (commands) calls this as ar_review::parse_repo_config; renaming it breaks \
              that crate"
)]
pub fn parse_repo_config(yaml: &str) -> Result<RepoConfig, serde_yaml::Error> {
    let config: RepoConfig = serde_yaml::from_str(yaml)?;
    config.github_automatic_review()?;
    Ok(config)
}

/// Strict parser: surfaces unknown top-level keys as errors so a
/// typo like `enabld: true` (missing `e`) is caught at validation
/// time instead of silently parsing as default values.
///
/// Layered over [`parse_repo_config`] rather than replacing it
/// because the runtime loader is intentionally permissive — a
/// repo that's pinned to an older `auto_review` version shouldn't
/// break when someone adds a forward-compat field. The strict
/// check is opt-in via `auto_review validate-config --strict`.
///
/// # Errors
///
/// Returns [`RepoConfigStrictError::UnknownKeys`] listing every
/// unrecognized key, or [`RepoConfigStrictError::Parse`] when the body is
/// not valid YAML or does not match the [`RepoConfig`] shape.
#[inline]
pub fn parse_repo_config_strict(yaml: &str) -> Result<RepoConfig, RepoConfigStrictError> {
    // First: parse generically and inspect top-level keys.
    let raw: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(RepoConfigStrictError::Parse)?;
    if let Some(map) = raw.as_mapping() {
        let mut unknown: Vec<String> = Vec::new();
        for (key, _) in map {
            if let Some(key_str) = key.as_str() {
                if !KNOWN_KEYS.contains(&key_str) {
                    unknown.push(key_str.to_owned());
                }
            }
        }

        if let Some(pr_metadata_check) =
            map.get(serde_yaml::Value::String("pr_metadata_check".into()))
        {
            if let Some(pr_metadata_check_map) = pr_metadata_check.as_mapping() {
                for (key, value) in pr_metadata_check_map {
                    if let Some(key_str) = key.as_str() {
                        if !KNOWN_PR_METADATA_CHECK_KEYS.contains(&key_str) {
                            unknown.push(format!("pr_metadata_check.{key_str}"));
                        }
                        if key_str == "checks" {
                            if let Some(checks_map) = value.as_mapping() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
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
            .and_then(|mapping| mapping.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(true));
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "both CLI validation modes must reject the fixture"
    )]
    fn invalid_github_admission_does_not_discard_other_runtime_settings() {
        let yaml = "enabled: false\ngithub_automatic_review:\n  required_checks:\n    Test: typo\n";
        assert!(parse_repo_config(yaml)
            .unwrap_err()
            .to_string()
            .contains("typo"));
        assert!(parse_repo_config_strict(yaml)
            .unwrap_err()
            .to_string()
            .contains("typo"));
        assert!(serde_yaml::from_str::<RepoConfig>(yaml)
            .is_ok_and(|config| !config.enabled && config.github_automatic_review().is_err()));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    fn parses_pr_metadata_check_false() {
        let cfg = parse_repo_config("pr_metadata_check: false\n").expect("parse config");
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|mapping| mapping.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    fn parses_object_pr_metadata_check_enabled_false() {
        let cfg =
            parse_repo_config("pr_metadata_check:\n  enabled: false\n").expect("parse config");
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let metadata_check = map
            .get(serde_yaml::Value::String("pr_metadata_check".into()))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|mapping| mapping.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn parses_override_approvers_list() {
        let cfg = parse_repo_config("override_approvers:\n  - jwilger\n  - alice\n")
            .expect("parse config");
        assert_eq!(cfg.override_approvers, vec!["jwilger", "alice"]);
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    fn default_override_approvers_is_empty_opt_in() {
        let cfg = RepoConfig::default();
        assert!(cfg.override_approvers.is_empty());
        // Opt-in: nobody is authorized when the list is empty.
        assert!(!cfg.is_override_approver("jwilger"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn override_approver_match_is_case_insensitive() {
        let cfg = parse_repo_config("override_approvers:\n  - JWilger\n").expect("parse config");
        assert!(cfg.is_override_approver("jwilger"));
        assert!(cfg.is_override_approver("JWILGER"));
        assert!(!cfg.is_override_approver("someone-else"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn parses_full_yaml_config() {
        let cfg = parse_repo_config(
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
        .expect("parse config");
        assert!(cfg.enabled);
        assert!(cfg.guidelines.contains("total functions"));
        assert_eq!(cfg.ignored_paths.len(), 2);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn enabled_false_disables_review() {
        let cfg = parse_repo_config("enabled: false\n").expect("parse config");
        assert!(!cfg.enabled);
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "the assertion stays as written; it checks only whether the call failed or succeeded"
    )]
    fn malformed_yaml_is_a_parse_error() {
        // The runtime loader turns this error into defaults plus a
        // warning; see trusted_files::tests.
        assert!(parse_repo_config("enabled: not_a_bool\n").is_err());
    }

    /// Contract test: the `KNOWN_KEYS` allow-list must match the
    /// fields on `RepoConfig` exactly. Adding a field to the
    /// struct without updating `KNOWN_KEYS` would make
    /// `parse_repo_config_strict` reject legitimate configs.
    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    fn strict_allowlist_matches_struct_fields() {
        // Round-trip a default config through serde_yaml as JSON
        // (which exposes field names) and confirm every key is in
        // the allow-list.
        let cfg = RepoConfig::default();
        let value = serde_yaml::to_value(&cfg).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised: alloc::collections::BTreeSet<&str> =
            map.iter().filter_map(|(key, _)| key.as_str()).collect();
        let allowed: alloc::collections::BTreeSet<&str> = KNOWN_KEYS.iter().copied().collect();
        assert_eq!(
            serialised, allowed,
            "RepoConfig fields and KNOWN_KEYS allow-list have drifted"
        );
    }

    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::shadow_unrelated,
        reason = "successive values are bound under the name the assertions read; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::similar_names,
        reason = "serialised_checks_keys and allowed_checks_keys are the two key sets the assertion compares; the parallel names say so"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    fn strict_metadata_allowlists_match_struct_fields() {
        let pr_metadata_check = PrMetadataCheck::default();
        let value = serde_yaml::to_value(&pr_metadata_check).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised_check_keys: alloc::collections::BTreeSet<&str> =
            map.iter().filter_map(|(key, _)| key.as_str()).collect();
        let allowed_check_keys: alloc::collections::BTreeSet<&str> =
            KNOWN_PR_METADATA_CHECK_KEYS.iter().copied().collect();
        assert_eq!(
            serialised_check_keys, allowed_check_keys,
            "PrMetadataCheck fields and KNOWN_PR_METADATA_CHECK_KEYS allow-list have drifted"
        );

        let pr_metadata_checks = PrMetadataChecks::default();
        let value = serde_yaml::to_value(&pr_metadata_checks).unwrap();
        let map = value.as_mapping().unwrap();
        let serialised_checks_keys: alloc::collections::BTreeSet<&str> =
            map.iter().filter_map(|(key, _)| key.as_str()).collect();
        let allowed_checks_keys: alloc::collections::BTreeSet<&str> =
            KNOWN_PR_METADATA_CHECKS_KEYS.iter().copied().collect();
        assert_eq!(
            serialised_checks_keys, allowed_checks_keys,
            "PrMetadataChecks fields and KNOWN_PR_METADATA_CHECKS_KEYS allow-list have drifted"
        );
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
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
            .and_then(|mapping| mapping.get(serde_yaml::Value::String("enabled".into())))
            .and_then(serde_yaml::Value::as_bool);
        assert_eq!(metadata_check, Some(false));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn strict_rejects_typo_in_top_level_key() {
        // Missing 'e' in 'enabled'.
        let yaml = "enabld: true\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        let msg = format!("{err}");
        assert!(msg.contains("enabld"), "{msg}");
        assert!(msg.contains("valid keys"), "{msg}");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn strict_rejects_retired_linter_keys() {
        let yaml = "mode: linter_only\ndisabled_tools:\n  - ruff\npre_merge_checks:\n  - old\n";
        let err = parse_repo_config_strict(yaml).expect_err("retired keys should fail");
        let msg = format!("{err}");
        assert!(msg.contains("disabled_tools"), "{msg}");
        assert!(msg.contains("mode"), "{msg}");
        assert!(msg.contains("pre_merge_checks"), "{msg}");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn strict_propagates_value_level_errors_through_serde() {
        let yaml = "enabled: not_a_bool\n";
        let err = parse_repo_config_strict(yaml).expect_err("should fail");
        assert!(matches!(err, RepoConfigStrictError::Parse(_)));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::semicolon_if_nothing_returned,
        reason = "the assertion stays as written"
    )]
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
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    fn partial_config_merges_with_defaults() {
        let cfg = parse_repo_config("ignored_paths:\n  - vendor/**\n").expect("parse config");
        // Unset fields keep their defaults.
        assert!(cfg.enabled);
        assert!(cfg.guidelines.is_empty());
        assert_eq!(cfg.ignored_paths, vec!["vendor/**"]);
    }
}
