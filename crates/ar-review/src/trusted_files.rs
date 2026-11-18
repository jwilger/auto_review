//! Trusted loader for repository-controlled instruction files.
//!
//! This module is the single loader for every file a repository uses to
//! instruct `auto_review`: `.auto_review.yaml` / `.auto_review.yml` today, and
//! later `AGENTS.md`, `CLAUDE.md`, `REVIEW.md` and path-scoped instructions.
//! Every such file is read from the pull request's **base branch** through
//! the repository host API, never from the head checkout. A pull request
//! therefore cannot change how `auto_review` reviews it: a PR that adds
//! `enabled: false` or `ignored_paths: ["**"]` only takes effect once it
//! has been merged into the base branch.
//!
//! New ingestion of repository-controlled instructions must go through
//! [`read_trusted_repo_file`] (or a base-tree read), not the workspace
//! clone. The module depends only on `ar_forge` host types (ADR-0020), so
//! Forgejo and GitHub hosts behave the same.

use crate::config::{parse_repo_config, RepoConfig};
use ar_forge::{HostError, ReviewHost};

/// Largest repository-controlled file body the trusted loader will parse.
///
/// The cap bounds YAML parsing, not the download: `ReviewHost::get_file_content`
/// returns the whole body, so an oversized file is fetched in full and then
/// rejected. Only someone with write access to the base branch controls that
/// body (see THREAT-MODEL T3a residual risk). Before this loader ingests files
/// whose size is less tightly controlled, the host transports should bound the
/// read itself (Content-Length check or a streaming limit).
pub const MAX_TRUSTED_FILE_BYTES: usize = 64 * 1024;

/// Repo config file names, in lookup order: `.yaml` wins over `.yml`.
pub const REPO_CONFIG_FILE_NAMES: [&str; 2] = [".auto_review.yaml", ".auto_review.yml"];

/// The base branch a pull request targets, as fetched once per review job.
///
/// `name` is the branch name trusted files are read at: reading the branch
/// tip by name means a config change merged to the base applies to open
/// pull requests on their next review. `sha` is the base tip the host
/// reported when the pull request was fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-orchestrator (dispatcher tests) and the red-team tests build BaseRef with a \
              struct literal; #[non_exhaustive] would break that construction"
)]
pub struct BaseRef {
    pub name: String,
    pub sha: String,
}

impl From<&ar_forge::PullRequestRefSummary> for BaseRef {
    #[inline]
    fn from(base: &ar_forge::PullRequestRefSummary) -> Self {
        Self {
            name: base.ref_name.clone(),
            sha: base.sha.clone(),
        }
    }
}

/// Why a trusted file could not be read.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TrustedFileError {
    /// The host returned an error other than "not found".
    #[error("failed to fetch {path} at {ref_name}: {source}")]
    Fetch {
        path: String,
        ref_name: String,
        #[source]
        source: HostError,
    },
    /// The body exceeds [`MAX_TRUSTED_FILE_BYTES`] and was not parsed.
    #[error("{path} at {ref_name} is {size} bytes, over the {limit}-byte limit; ignoring it")]
    TooLarge {
        path: String,
        ref_name: String,
        size: usize,
        limit: usize,
    },
}

/// Where a loaded [`RepoConfig`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigProvenance {
    /// Parsed from `path` on the base branch `ref_name`.
    BaseBranch { path: String, ref_name: String },
    /// Built-in defaults: no config on the base branch, or it could not be
    /// fetched or parsed (see [`LoadedRepoConfig::warning`]).
    Default,
}

/// A repo config together with where it came from and any fallback warning.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LoadedRepoConfig {
    pub config: RepoConfig,
    pub provenance: ConfigProvenance,
    /// Set when the loader fell back to defaults because the base-branch
    /// config was unreadable, oversized or unparsable. `None` when the
    /// config loaded or simply does not exist.
    pub warning: Option<String>,
}

impl LoadedRepoConfig {
    /// Built-in defaults with an optional fallback `warning`.
    fn defaults(warning: Option<String>) -> Self {
        Self {
            config: RepoConfig::default(),
            provenance: ConfigProvenance::Default,
            warning,
        }
    }
}

/// Read a repository-controlled file from `base_ref` through the host API.
///
/// Returns `Ok(None)` when the file does not exist at that ref. Bodies over
/// [`MAX_TRUSTED_FILE_BYTES`] are rejected with [`TrustedFileError::TooLarge`].
///
/// # Errors
///
/// Returns [`TrustedFileError::Fetch`] when the host fails for a reason other
/// than "not found", and [`TrustedFileError::TooLarge`] for an oversized body.
#[inline]
pub async fn read_trusted_repo_file<H>(
    host: &H,
    owner: &str,
    repo: &str,
    path: &str,
    base_ref: &str,
) -> Result<Option<String>, TrustedFileError>
where
    H: ReviewHost + ?Sized,
{
    let fetched = host
        .get_file_content(owner, repo, path, base_ref)
        .await
        .map_err(|source| TrustedFileError::Fetch {
            path: path.to_owned(),
            ref_name: base_ref.to_owned(),
            source,
        })?;
    match fetched {
        Some(body) if body.len() > MAX_TRUSTED_FILE_BYTES => Err(TrustedFileError::TooLarge {
            path: path.to_owned(),
            ref_name: base_ref.to_owned(),
            size: body.len(),
            limit: MAX_TRUSTED_FILE_BYTES,
        }),
        other => Ok(other),
    }
}

/// Load `.auto_review.yaml` (then `.auto_review.yml`) from the base branch
/// `base_ref` through the host API.
///
/// Never fails: a missing config yields defaults with no warning; a fetch
/// error, an oversized body or an unparsable body yields defaults with a
/// warning (also emitted through `tracing::warn!`) so the review proceeds.
#[inline]
pub async fn load_repo_config_from_base<H>(
    host: &H,
    owner: &str,
    repo: &str,
    base_ref: &str,
) -> LoadedRepoConfig
where
    H: ReviewHost + ?Sized,
{
    for path in REPO_CONFIG_FILE_NAMES {
        match read_trusted_repo_file(host, owner, repo, path, base_ref).await {
            Ok(None) => {}
            Ok(Some(body)) => {
                return match parse_repo_config(&body) {
                    Ok(config) => LoadedRepoConfig {
                        config,
                        provenance: ConfigProvenance::BaseBranch {
                            path: path.to_owned(),
                            ref_name: base_ref.to_owned(),
                        },
                        warning: None,
                    },
                    Err(error) => {
                        let warning = format!(
                            "failed to parse {path} at {base_ref}: {error}; using defaults"
                        );
                        tracing::warn!(%owner, %repo, %warning, "repo config fallback");
                        LoadedRepoConfig::defaults(Some(warning))
                    }
                };
            }
            Err(error) => {
                let warning = format!("{error}; using defaults");
                tracing::warn!(%owner, %repo, %warning, "repo config fallback");
                return LoadedRepoConfig::defaults(Some(warning));
            }
        }
    }
    LoadedRepoConfig::defaults(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_forge::{
        ChangedFile, CommitStatus, CreateReviewRequest, CreatedReview, PrReviewComment,
        PullRequestSummary, PullReviewSummary,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Serves base-branch files from a map keyed by `(ref, path)`.
    #[derive(Default)]
    struct FakeHost {
        fail_with: Option<String>,
        files: HashMap<(String, String), String>,
        requests: Mutex<Vec<(String, String)>>,
    }

    impl FakeHost {
        fn with_file(mut self, ref_name: &str, path: &str, body: &str) -> Self {
            self.files
                .insert((ref_name.to_owned(), path.to_owned()), body.to_owned());
            self
        }
    }

    #[async_trait]
    impl ReviewHost for FakeHost {
        async fn create_review(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: &CreateReviewRequest,
        ) -> Result<CreatedReview, HostError> {
            Err(unused())
        }

        async fn get_compare_diff(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<String, HostError> {
            Err(unused())
        }

        #[expect(
            clippy::pattern_type_mismatch,
            reason = "the test matches borrowed fixtures and recorded values with default binding modes; explicit & patterns would edit the matched shapes the assertions read"
        )]
        #[expect(
            clippy::unwrap_used,
            reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
        )]
        async fn get_file_content(
            &self,
            _owner: &str,
            _repo: &str,
            file_path: &str,
            ref_: &str,
        ) -> Result<Option<String>, HostError> {
            self.requests
                .lock()
                .unwrap()
                .push((ref_.to_owned(), file_path.to_owned()));
            if let Some(message) = &self.fail_with {
                return Err(HostError::new(message.clone()));
            }
            Ok(self
                .files
                .get(&(ref_.to_owned(), file_path.to_owned()))
                .cloned())
        }

        async fn get_pr_diff(&self, _: &str, _: &str, _: u64) -> Result<String, HostError> {
            Err(unused())
        }

        async fn get_pull_request(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<PullRequestSummary, HostError> {
            Err(unused())
        }

        async fn list_changed_files(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<ChangedFile>, HostError> {
            Err(unused())
        }

        async fn list_pr_review_comments(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(unused())
        }

        async fn list_pull_review_comments(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(unused())
        }

        async fn list_pull_reviews(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<PullReviewSummary>, HostError> {
            Err(unused())
        }

        async fn post_commit_status(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &CommitStatus,
        ) -> Result<(), HostError> {
            Err(unused())
        }

        async fn update_pull_request(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<(), HostError> {
            Err(unused())
        }
    }

    fn unused() -> HostError {
        HostError::new("not used in trusted_files tests")
    }

    async fn load(host: &FakeHost) -> LoadedRepoConfig {
        load_repo_config_from_base(host, "o", "r", "main").await
    }

    #[tokio::test]
    async fn loads_yaml_from_base_ref_with_provenance() {
        let host = FakeHost::default().with_file("main", ".auto_review.yaml", "enabled: false\n");
        let loaded = load(&host).await;
        assert!(!loaded.config.enabled);
        assert_eq!(
            loaded.provenance,
            ConfigProvenance::BaseBranch {
                path: ".auto_review.yaml".into(),
                ref_name: "main".into()
            }
        );
        assert_eq!(loaded.warning, None);
    }

    #[tokio::test]
    #[expect(
        clippy::str_to_string,
        reason = "the expected values in this test's assertions build their strings with to_string and stay as written"
    )]
    async fn loads_yml_when_only_yml_exists() {
        let host = FakeHost::default().with_file(
            "main",
            ".auto_review.yml",
            "ignored_paths: [\"docs/**\"]\n",
        );
        let loaded = load(&host).await;
        assert_eq!(loaded.config.ignored_paths, vec!["docs/**".to_string()]);
        assert_eq!(
            loaded.provenance,
            ConfigProvenance::BaseBranch {
                path: ".auto_review.yml".into(),
                ref_name: "main".into()
            }
        );
        assert_eq!(loaded.warning, None);
    }

    #[tokio::test]
    async fn yaml_wins_when_both_exist() {
        let host = FakeHost::default()
            .with_file("main", ".auto_review.yaml", "guidelines: from-yaml\n")
            .with_file("main", ".auto_review.yml", "guidelines: from-yml\n");
        let loaded = load(&host).await;
        assert_eq!(loaded.config.guidelines, "from-yaml");
    }

    #[tokio::test]
    #[expect(
        clippy::str_to_string,
        reason = "the expected values in this test's assertions build their strings with to_string and stay as written"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn neither_file_yields_defaults_without_warning() {
        let host = FakeHost::default();
        let loaded = load(&host).await;
        assert!(loaded.config.enabled);
        assert_eq!(loaded.provenance, ConfigProvenance::Default);
        assert_eq!(loaded.warning, None);
        assert_eq!(
            *host.requests.lock().unwrap(),
            vec![
                ("main".to_string(), ".auto_review.yaml".to_string()),
                ("main".to_string(), ".auto_review.yml".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn reads_only_the_named_base_ref() {
        let host =
            FakeHost::default().with_file("feature", ".auto_review.yaml", "enabled: false\n");
        let loaded = load(&host).await;
        assert!(loaded.config.enabled);
        assert_eq!(loaded.provenance, ConfigProvenance::Default);
    }

    #[tokio::test]
    #[expect(
        clippy::decimal_literal_representation,
        reason = "the asserted byte counts are the 64 KiB cap and one byte over it, written in decimal; the assertions stay as written"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn body_over_limit_falls_back_to_defaults_with_warning() {
        let mut body = "enabled: false\n".to_owned();
        body.push_str(&"#".repeat(MAX_TRUSTED_FILE_BYTES + 1 - body.len()));
        assert_eq!(body.len(), 65_537);
        let host = FakeHost::default().with_file("main", ".auto_review.yaml", &body);
        let loaded = load(&host).await;
        assert!(loaded.config.enabled);
        assert_eq!(loaded.provenance, ConfigProvenance::Default);
        let warning = loaded.warning.expect("oversized config warns");
        assert!(
            warning.contains("65536"),
            "warning names the limit: {warning}"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::decimal_literal_representation,
        reason = "the asserted byte counts are the 64 KiB cap and one byte over it, written in decimal; the assertions stay as written"
    )]
    async fn body_at_exact_limit_parses() {
        let mut body = "enabled: false\n".to_owned();
        body.push_str(&"#".repeat(MAX_TRUSTED_FILE_BYTES - body.len()));
        assert_eq!(body.len(), 65_536);
        let host = FakeHost::default().with_file("main", ".auto_review.yaml", &body);
        let loaded = load(&host).await;
        assert!(!loaded.config.enabled);
        assert_eq!(loaded.warning, None);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn fetch_error_falls_back_to_defaults_with_warning() {
        let host = FakeHost {
            fail_with: Some("HTTP 500".into()),
            ..FakeHost::default()
        };
        let loaded = load(&host).await;
        assert!(loaded.config.enabled);
        assert_eq!(loaded.provenance, ConfigProvenance::Default);
        let warning = loaded.warning.expect("fetch error warns");
        assert!(warning.contains("HTTP 500"), "{warning}");
    }

    #[tokio::test]
    async fn unparsable_config_falls_back_to_defaults_with_warning() {
        let host =
            FakeHost::default().with_file("main", ".auto_review.yaml", "enabled: [not a bool\n");
        let loaded = load(&host).await;
        assert!(loaded.config.enabled);
        assert_eq!(loaded.provenance, ConfigProvenance::Default);
        assert!(loaded.warning.is_some());
    }

    #[test]
    fn base_ref_comes_from_pull_request_base() {
        let base = ar_forge::PullRequestRefSummary {
            ref_name: "release/1.x".into(),
            sha: "cafef00d".into(),
        };
        assert_eq!(
            BaseRef::from(&base),
            BaseRef {
                name: "release/1.x".into(),
                sha: "cafef00d".into()
            }
        );
    }

    #[tokio::test]
    async fn read_trusted_repo_file_returns_none_when_missing() {
        let host = FakeHost::default();
        let body = read_trusted_repo_file(&host, "o", "r", "REVIEW.md", "main").await;
        assert_eq!(body, Ok(None));
    }
}
