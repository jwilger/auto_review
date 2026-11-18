//! Repo-clone helpers for the review pipeline.
//!
//! Linters need files on disk, not just a diff. `prepare_workspace` shallow-
//! clones the target repo into a temp directory, fetches the PR's head SHA,
//! checks it out, and removes the credential-bearing `origin` remote. The
//! returned [`PreparedWorkspace`] auto-deletes its tempdir when dropped.

use crate::error::ReviewError;
use ar_forge::ChangedFile;
use std::{env, ffi::OsStr, path::Path};
use tempfile::TempDir;
use tokio::process::Command;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("failed to set credentials on URL")]
    CredentialEncoding,
    #[error("git error: {0}")]
    Git(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<WorkspaceError> for ReviewError {
    fn from(e: WorkspaceError) -> Self {
        ReviewError::Workspace(e.to_string())
    }
}

/// A cloned repository on local disk, alive for as long as the value lives.
#[derive(Debug)]
pub struct PreparedWorkspace {
    dir: TempDir,
    head_sha: String,
    incremental_base: Option<String>,
    merge_base: Option<String>,
}

impl PreparedWorkspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The merge base of the pull request's base and head commits, when
    /// the workspace was prepared with a base SHA and the merge base was
    /// found within the bounded history fetch.
    pub fn merge_base(&self) -> Option<&str> {
        self.merge_base.as_deref()
    }

    /// Unified diff of the pull request from its merge base to its head:
    /// the same comparison GitHub and Forgejo show for a pull request, so
    /// new-side line numbers match the host's inline-comment positions.
    ///
    /// Git's own binary detection applies: a binary file appears as a
    /// `Binary files ... differ` line, never with its content. External
    /// diff drivers and textconv filters are disabled.
    ///
    /// Errors when the workspace has no merge base (prepared without a
    /// base SHA, or the bounded history fetch found none).
    pub async fn pr_diff(&self) -> Result<String, WorkspaceError> {
        let merge_base = self.require_merge_base()?;
        self.diff_to_head(merge_base, merge_base).await
    }

    /// Unified diff of the commits pushed since an earlier review: from the
    /// merge base of the previously reviewed commit and the head, to the
    /// head. The same comparison as the host's `previous...head` compare
    /// view, with the binary, rename and driver rules of [`Self::pr_diff`].
    /// Attributes still come from the pull request's merge base, because
    /// the previously reviewed commit is pull-request content.
    ///
    /// Errors when the workspace was not prepared through
    /// [`prepare_workspace_from_clone_url_with_previous`], or the previous
    /// commit or either merge base could not be found within the bounded
    /// history fetch (for example after a force push removed it).
    pub async fn incremental_diff(&self) -> Result<String, WorkspaceError> {
        let merge_base = self.require_merge_base()?;
        let incremental_base = self.incremental_base.as_deref().ok_or_else(|| {
            WorkspaceError::Git(
                "no incremental base: the previously reviewed commit is not in the workspace"
                    .into(),
            )
        })?;
        self.diff_to_head(incremental_base, merge_base).await
    }

    /// `git diff <from> <head>` with attributes read from `attributes_tree`.
    async fn diff_to_head(
        &self,
        from: &str,
        attributes_tree: &str,
    ) -> Result<String, WorkspaceError> {
        let stdout = git_stdout_in(
            self.path(),
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                from,
                &self.head_sha,
            ],
            &attributes_from(attributes_tree),
        )
        .await?;
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Files the pull request changes, merge base to head, in the host's
    /// vocabulary (`added`, `modified`, `removed`, `renamed`, `copied`,
    /// `changed`). Computed locally, so no host file-count cap applies.
    ///
    /// Binary files are listed with zero additions and deletions; renamed
    /// and copied files carry their new path. `patch` is always `None`: the
    /// hunks come from [`Self::pr_diff`].
    pub async fn pr_changed_files(&self) -> Result<Vec<ChangedFile>, WorkspaceError> {
        let merge_base = self.require_merge_base()?;
        let attributes = attributes_from(merge_base);
        let name_status = git_stdout_in(
            self.path(),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--name-status",
                "-z",
                merge_base,
                &self.head_sha,
            ],
            &attributes,
        )
        .await?;
        let numstat = git_stdout_in(
            self.path(),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--numstat",
                "-z",
                merge_base,
                &self.head_sha,
            ],
            &attributes,
        )
        .await?;
        let counts = parse_numstat_z(&numstat);
        Ok(parse_name_status_z(&name_status)
            .into_iter()
            .map(|(status, filename)| {
                let (additions, deletions) = counts
                    .iter()
                    .find(|(path, _)| *path == filename)
                    .map_or((0, 0), |(_, lines)| *lines);
                ChangedFile {
                    additions,
                    changes: additions.saturating_add(deletions),
                    deletions,
                    filename,
                    patch: None,
                    status: status.to_owned(),
                }
            })
            .collect())
    }

    fn require_merge_base(&self) -> Result<&str, WorkspaceError> {
        self.merge_base().ok_or_else(|| {
            WorkspaceError::Git(
                "no merge base: the workspace cannot compute the pull request diff".into(),
            )
        })
    }
}

/// Build the authenticated clone URL for a given Forgejo `(base, owner, repo)`.
///
/// Uses Forgejo's HTTP-basic-style token auth: `https://oauth2:<token>@host/...`.
/// Doesn't validate the token; just URL-encodes it into userinfo.
pub fn build_clone_url(
    base: &str,
    owner: &str,
    repo: &str,
    token: &str,
) -> Result<String, WorkspaceError> {
    let mut u = Url::parse(base).map_err(|_| WorkspaceError::InvalidBaseUrl(base.into()))?;
    let new_path = {
        let trimmed = u.path().trim_end_matches('/');
        format!("{trimmed}/{owner}/{repo}.git")
    };
    u.set_path(&new_path);
    u.set_username("oauth2")
        .map_err(|_| WorkspaceError::CredentialEncoding)?;
    u.set_password(Some(token))
        .map_err(|_| WorkspaceError::CredentialEncoding)?;
    Ok(u.to_string())
}

/// Clone the repo at `head_sha` into a fresh tempdir and return its path.
///
/// Strategy: `git clone --no-checkout --depth=1`, then `git fetch --depth=1
/// origin <sha>`, then `git checkout <sha>`. This works even when the head
/// SHA isn't on the default branch. The `origin` remote is then removed so
/// the token never stays in the workspace `.git/config`.
pub async fn prepare_workspace(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    head_sha: &str,
) -> Result<PreparedWorkspace, WorkspaceError> {
    let url = build_clone_url(base, owner, repo, token)?;
    prepare_workspace_from_clone_url(&url, head_sha).await
}

/// Clone an already-authenticated repository URL at `head_sha`.
///
/// This is the provider-neutral clone seam: host adapters prepare their own
/// clone URL/auth material, while workspace preparation keeps the same hermetic
/// git execution and SHA validation.
///
/// The `origin` remote is removed right after checkout so the workspace
/// `.git/config` holds no credential-bearing URL. Any further fetch from the
/// clone source must happen before that removal.
pub async fn prepare_workspace_from_clone_url(
    clone_url: &str,
    head_sha: &str,
) -> Result<PreparedWorkspace, WorkspaceError> {
    clone_workspace(clone_url, head_sha, None, None).await
}

/// Clone an already-authenticated repository URL at `head_sha` and also
/// fetch enough of `base_sha`'s history to find the pull request's merge
/// base, so [`PreparedWorkspace::pr_diff`] can compute the diff the host
/// shows for the pull request without the host's whole-diff endpoint.
///
/// Both SHAs are validated before git runs. The base history is fetched
/// before the credential-bearing `origin` remote is removed, deepening in
/// the bounded steps of [`MERGE_BASE_DEEPEN_STEPS`]. When the base cannot be
/// fetched or no merge base appears within that bound, the workspace is
/// still returned with [`PreparedWorkspace::merge_base`] `None`, so callers
/// keep the checkout for context and fall back to the host's diff.
pub async fn prepare_workspace_from_clone_url_with_base(
    clone_url: &str,
    head_sha: &str,
    base_sha: &str,
) -> Result<PreparedWorkspace, WorkspaceError> {
    clone_workspace(clone_url, head_sha, Some(base_sha), None).await
}

/// Like [`prepare_workspace_from_clone_url_with_base`], and also fetch the
/// commit an earlier review covered (`previous_sha`), so
/// [`PreparedWorkspace::incremental_diff`] can show only what was pushed
/// since without the host's compare endpoint.
///
/// All three SHAs are validated before git runs. The previous commit is
/// fetched with the same bounded deepening as the base, and only when the
/// pull request's merge base was found. When it cannot be fetched (a force
/// push removed it) or no merge base with the head appears within the
/// bound, the workspace is still returned and `incremental_diff` errors,
/// so callers fall back to the host or to a full review.
pub async fn prepare_workspace_from_clone_url_with_previous(
    clone_url: &str,
    head_sha: &str,
    base_sha: &str,
    previous_sha: &str,
) -> Result<PreparedWorkspace, WorkspaceError> {
    clone_workspace(clone_url, head_sha, Some(base_sha), Some(previous_sha)).await
}

/// The most extra history, in commits behind each tip, a review fetches
/// while looking for a merge base. It bounds the clone, so a pull request
/// against a huge-history repository (or unrelated histories) cannot stall
/// a review or turn the shallow clone into a full one.
pub const MERGE_BASE_HISTORY_CAP: u32 = 1000;

/// How many more commits each deepening round fetches behind the head and
/// base while looking for their merge base. Small first steps keep the
/// common case (a recent fork point) cheap; the steps sum to
/// [`MERGE_BASE_HISTORY_CAP`].
pub const MERGE_BASE_DEEPEN_STEPS: &[u32] = &[64, 256, 680];

async fn clone_workspace(
    clone_url: &str,
    head_sha: &str,
    base_sha: Option<&str>,
    previous_sha: Option<&str>,
) -> Result<PreparedWorkspace, WorkspaceError> {
    // Defence in depth: head_sha is interpolated into git argv
    // (`git fetch origin <sha>`, `git checkout <sha>`) and git
    // accepts intermingled `--option`-style args after positional
    // ones. A pathological value like `--upload-pack=...` would be
    // re-interpreted as a flag, not a refspec. Forgejo always
    // sends well-formed SHAs and the HMAC-verified webhook is the
    // only entry point for this value, but rejecting non-hex
    // input here keeps the clone path robust against any future
    // intake (replay tooling, manual review-once invocations,
    // etc.) that might supply less-trusted strings.
    if !is_valid_git_sha(head_sha) {
        return Err(WorkspaceError::Git(format!(
            "refusing to clone: head_sha {head_sha:?} is not a hex commit SHA"
        )));
    }
    if let Some(base_sha) = base_sha.filter(|sha| !is_valid_git_sha(sha)) {
        return Err(WorkspaceError::Git(format!(
            "refusing to clone: base_sha {base_sha:?} is not a hex commit SHA"
        )));
    }
    if let Some(previous_sha) = previous_sha.filter(|sha| !is_valid_git_sha(sha)) {
        return Err(WorkspaceError::Git(format!(
            "refusing to clone: previous_sha {previous_sha:?} is not a hex commit SHA"
        )));
    }
    let dir = TempDir::new()?;
    let path = dir.path().to_owned();

    git(&[
        OsStr::new("clone"),
        OsStr::new("--no-checkout"),
        OsStr::new("--depth=1"),
        OsStr::new(clone_url),
        path.as_os_str(),
    ])
    .await?;
    git_in(&path, &["fetch", "--depth=1", "origin", head_sha]).await?;
    git_in(&path, &["checkout", head_sha]).await?;
    let merge_base = match base_sha {
        Some(base_sha) => {
            locate_merge_base(&path, head_sha, base_sha, MERGE_BASE_DEEPEN_STEPS).await
        }
        None => None,
    };
    // Only with the pull request's merge base: it is the trusted attribute
    // source an incremental diff needs.
    let incremental_base = match (merge_base.as_ref(), previous_sha) {
        (Some(_), Some(previous_sha)) => {
            locate_merge_base(&path, head_sha, previous_sha, MERGE_BASE_DEEPEN_STEPS).await
        }
        _ => None,
    };
    // The clone URL carries the bot credential in its userinfo and `git
    // clone` stores it as `remote.origin.url` in `.git/config`, which the
    // LLM-driven `read_file` tool can reach. Drop the remote now that the
    // checkout is done. On failure `dir` is dropped here, so no workspace
    // that still holds the credential escapes (fail closed).
    git_in(&path, &["remote", "remove", "origin"]).await?;

    Ok(PreparedWorkspace {
        dir,
        head_sha: head_sha.to_owned(),
        incremental_base,
        merge_base,
    })
}

/// Fetch `base_sha` into the shallow clone at `path` and deepen both tips,
/// one entry of `deepen_steps` at a time, until git finds their merge base.
///
/// Returns `None` when the base cannot be fetched, a deepening fetch fails,
/// the history is complete without a common ancestor, or the steps run out.
/// Each failure is logged; none fails workspace preparation.
async fn locate_merge_base(
    path: &Path,
    head_sha: &str,
    base_sha: &str,
    deepen_steps: &[u32],
) -> Option<String> {
    if let Err(err) = git_in(path, &["fetch", "--depth=1", "origin", base_sha]).await {
        tracing::warn!(error = %err, "could not fetch the pull request base commit");
        return None;
    }
    for step in deepen_steps {
        if let Some(merge_base) = merge_base_of(path, head_sha, base_sha).await {
            return Some(merge_base);
        }
        if !is_shallow(path).await {
            tracing::warn!("base and head share no history; no merge base");
            return None;
        }
        let deepen = format!("--deepen={step}");
        if let Err(err) = git_in(path, &["fetch", &deepen, "origin", head_sha, base_sha]).await {
            tracing::warn!(error = %err, "could not deepen history to find the merge base");
            return None;
        }
    }
    let merge_base = merge_base_of(path, head_sha, base_sha).await;
    if merge_base.is_none() {
        tracing::warn!(
            steps = ?deepen_steps,
            "no merge base within the bounded history fetch"
        );
    }
    merge_base
}

/// `git merge-base <base> <head>`, or `None` when git finds none in the
/// history fetched so far.
async fn merge_base_of(path: &Path, head_sha: &str, base_sha: &str) -> Option<String> {
    let stdout = git_stdout_in(path, &["merge-base", base_sha, head_sha], &[])
        .await
        .ok()?;
    let merge_base = String::from_utf8_lossy(&stdout).trim().to_owned();
    is_valid_git_sha(&merge_base).then_some(merge_base)
}

/// NUL-separated fields of git's `-z` output, minus the terminator after
/// the last record. Empty fields inside a record are kept: `--numstat -z`
/// uses one to introduce a rename's two paths.
fn z_fields(stdout: &[u8]) -> impl Iterator<Item = String> + '_ {
    stdout
        .strip_suffix(b"\0")
        .unwrap_or(stdout)
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned())
}

/// Parse `git diff --name-status -z` into `(host status, path)` pairs.
/// Renames and copies carry two paths; the new path is kept.
fn parse_name_status_z(stdout: &[u8]) -> Vec<(&'static str, String)> {
    let mut fields = z_fields(stdout);
    let mut files = Vec::new();
    while let Some(code) = fields.next() {
        let two_paths = code.starts_with('R') || code.starts_with('C');
        let path = if two_paths {
            fields.next();
            fields.next()
        } else {
            fields.next()
        };
        let Some(path) = path else { break };
        files.push((host_status(&code), path));
    }
    files
}

/// Map a git status letter to the host's changed-file status vocabulary
/// (GitHub's `pulls/{n}/files`), which triage and path filtering expect.
fn host_status(code: &str) -> &'static str {
    match code.chars().next() {
        Some('A') => "added",
        Some('D') => "removed",
        Some('R') => "renamed",
        Some('C') => "copied",
        Some('T') => "changed",
        _ => "modified",
    }
}

/// Parse `git diff --numstat -z` into `(path, (additions, deletions))`.
/// Binary files report `-` for both counts and become zero. Renames and
/// copies end the counts field with an empty path and carry the old and
/// new paths as the next two fields; the new path is kept.
fn parse_numstat_z(stdout: &[u8]) -> Vec<(String, (u32, u32))> {
    let mut fields = z_fields(stdout);
    let mut files = Vec::new();
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let additions = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let deletions = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let path = match parts.next() {
            Some(path) if !path.is_empty() => Some(path.to_owned()),
            _ => {
                fields.next();
                fields.next()
            }
        };
        let Some(path) = path else { break };
        files.push((path, (additions, deletions)));
    }
    files
}

/// Environment that makes git read `.gitattributes` from `tree` instead of
/// the head checkout. Without it a pull request could add `path -diff` and
/// show its own text changes as `Binary files ... differ`, hiding them from
/// review. The merge base is on the base branch, which the pull request
/// cannot change (the same trust rule as ADR-0024's base-branch config).
fn attributes_from(tree: &str) -> [(&'static str, &str); 1] {
    [("GIT_ATTR_SOURCE", tree)]
}

/// Whether the clone at `path` still has a shallow history boundary.
async fn is_shallow(path: &Path) -> bool {
    git_stdout_in(path, &["rev-parse", "--is-shallow-repository"], &[])
        .await
        .map_or(true, |stdout| stdout.trim_ascii() != b"false")
}

/// Recognise a git SHA: 7–64 hex characters (covers abbreviated
/// SHAs, full SHA-1 = 40 chars, and SHA-256 = 64 chars). Used by
/// `prepare_workspace` to reject argv-injection attempts before
/// the value reaches git.
pub fn is_valid_git_sha(s: &str) -> bool {
    let len = s.len();
    (7..=64).contains(&len) && s.chars().all(|c| c.is_ascii_hexdigit())
}

async fn git<S: AsRef<OsStr>>(args: &[S]) -> Result<(), WorkspaceError> {
    let output = hermetic_git_command().args(args).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(WorkspaceError::Git(redact_token(&stderr)));
    }
    Ok(())
}

async fn git_in(dir: &Path, args: &[&str]) -> Result<(), WorkspaceError> {
    git_stdout_in(dir, args, &[]).await.map(drop)
}

/// Run hermetic git in `dir` with extra environment `envs` and return its
/// stdout.
async fn git_stdout_in(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Vec<u8>, WorkspaceError> {
    let output = hermetic_git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(WorkspaceError::Git(redact_token(&stderr)));
    }
    Ok(output.stdout)
}

fn hermetic_git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_NOSYSTEM", "true")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", "/dev/null")
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_ASKPASS")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_EXEC_PATH")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_TEMPLATE_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("SSH_ASKPASS");
    for (key, _) in env::vars_os() {
        if key == "GIT_CONFIG_COUNT"
            || key == "GIT_CONFIG_PARAMETERS"
            || key == "GIT_CONFIG_SYSTEM"
            || key.to_string_lossy().starts_with("GIT_CONFIG_KEY_")
            || key.to_string_lossy().starts_with("GIT_CONFIG_VALUE_")
        {
            command.env_remove(key);
        }
    }
    command
}

/// Strip any `oauth2:<token>@` userinfo from git error output so logs don't
/// leak the bot's PAT.
fn redact_token(s: &str) -> String {
    const NEEDLE: &str = "oauth2:";
    const REPLACEMENT: &str = "oauth2:***";
    let mut out = s.to_string();
    let mut cursor = 0;
    while cursor < out.len() {
        let Some(rel) = out[cursor..].find(NEEDLE) else {
            break;
        };
        let start = cursor + rel;
        let Some(at_rel) = out[start..].find('@') else {
            break;
        };
        out.replace_range(start..start + at_rel, REPLACEMENT);
        cursor = start + REPLACEMENT.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn clone_url_uses_oauth2_userinfo() {
        let url =
            build_clone_url("https://forgejo.example.com", "alice", "widgets", "tok123").unwrap();
        assert_eq!(
            url,
            "https://oauth2:tok123@forgejo.example.com/alice/widgets.git"
        );
    }

    #[test]
    fn clone_url_handles_base_with_trailing_slash() {
        let url = build_clone_url("https://forgejo.example.com/", "a", "b", "tok").unwrap();
        assert_eq!(url, "https://oauth2:tok@forgejo.example.com/a/b.git");
    }

    #[test]
    fn clone_url_preserves_port() {
        let url = build_clone_url("https://git.local:3000", "a", "b", "tok").unwrap();
        assert_eq!(url, "https://oauth2:tok@git.local:3000/a/b.git");
    }

    #[test]
    fn clone_url_preserves_subpath() {
        let url = build_clone_url("https://example.com/git", "a", "b", "tok").unwrap();
        assert_eq!(url, "https://oauth2:tok@example.com/git/a/b.git");
    }

    #[test]
    fn clone_url_url_encodes_token_with_special_characters() {
        let url = build_clone_url("https://x.example", "a", "b", "to/k:n#1").unwrap();
        // ':' '/' '#' all need percent-encoding inside userinfo password.
        assert!(url.contains("oauth2:to%2Fk%3An%231@x.example"));
    }

    #[test]
    fn invalid_base_url_returns_error() {
        let err = build_clone_url("not a url", "a", "b", "t").expect_err("err");
        assert!(matches!(err, WorkspaceError::InvalidBaseUrl(_)));
    }

    #[test]
    fn redact_token_strips_password_segment() {
        let raw = "fatal: unable to access 'https://oauth2:secret123@host/a/b.git/': error";
        let redacted = redact_token(raw);
        assert!(!redacted.contains("secret123"));
        assert!(redacted.contains("oauth2:***@"));
    }

    #[test]
    fn redact_token_handles_string_without_userinfo() {
        let raw = "fatal: not a git repository";
        let redacted = redact_token(raw);
        assert_eq!(redacted, raw);
    }

    #[test]
    fn is_valid_git_sha_accepts_real_shas() {
        assert!(is_valid_git_sha("deadbeef")); // 8 chars
        assert!(is_valid_git_sha("0000000000000000000000000000000000000000")); // 40 hex (SHA-1)
        assert!(is_valid_git_sha(
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        )); // 64 hex (SHA-256)
    }

    #[test]
    fn is_valid_git_sha_rejects_argv_injection_attempts() {
        // The whole point of the validator: refuse anything that
        // looks like a git option flag.
        assert!(!is_valid_git_sha("--upload-pack=evil"));
        assert!(!is_valid_git_sha("--exec=touch /tmp/owned"));
        // Includes a slash → not a SHA.
        assert!(!is_valid_git_sha("../etc/passwd"));
        // Empty / too short / too long.
        assert!(!is_valid_git_sha(""));
        assert!(!is_valid_git_sha("abc"));
        assert!(!is_valid_git_sha(&"a".repeat(65)));
        // Non-hex chars.
        assert!(!is_valid_git_sha("notahexsha"));
        assert!(!is_valid_git_sha("0xdeadbeef"));
    }

    #[tokio::test]
    async fn prepare_workspace_rejects_non_sha_head_before_invoking_git() {
        // The clone+fetch should never run when the SHA is a
        // pathological value. We can't easily mock git here, but
        // any non-hex value will short-circuit before reaching
        // the Command::new("git") call. The error string surfaces
        // the offending value back to the caller.
        let err = prepare_workspace(
            "https://forgejo.example.com",
            "tok",
            "alice",
            "widgets",
            "--upload-pack=evil",
        )
        .await
        .expect_err("must reject");
        match err {
            WorkspaceError::Git(msg) => {
                assert!(
                    msg.contains("not a hex commit SHA"),
                    "expected validation message, got: {msg}"
                );
                assert!(
                    msg.contains("upload-pack"),
                    "expected the bad value echoed for diagnosis"
                );
            }
            other => panic!("unexpected error class: {other:?}"),
        }
    }

    #[tokio::test]
    async fn prepare_workspace_from_clone_url_rejects_non_sha_head_before_invoking_git() {
        let err = prepare_workspace_from_clone_url(
            "https://oauth2:tok@forgejo.example.com/alice/widgets.git",
            "--upload-pack=evil",
        )
        .await
        .expect_err("must reject");

        match err {
            WorkspaceError::Git(msg) => {
                assert!(
                    msg.contains("not a hex commit SHA"),
                    "expected validation message, got: {msg}"
                );
                assert!(
                    msg.contains("upload-pack"),
                    "expected the bad value echoed for diagnosis"
                );
            }
            other => panic!("unexpected error class: {other:?}"),
        }
    }

    #[tokio::test]
    async fn git_helper_ignores_ambient_global_git_aliases() {
        let _guard = ENV_LOCK.lock().await;

        let temp = TempDir::new().unwrap();
        let sentinel = temp.path().join("ambient-git-alias-ran");
        let global_config = temp.path().join("host.gitconfig");
        let quoted_sentinel = shell_single_quote(&sentinel.display().to_string());
        fs::write(
            &global_config,
            format!("[alias]\n\towned = !touch {quoted_sentinel}\n"),
        )
        .unwrap();

        let previous_global_config = env::var_os("GIT_CONFIG_GLOBAL");
        env::set_var("GIT_CONFIG_GLOBAL", &global_config);

        let result = git(&["owned"]).await;

        match previous_global_config {
            Some(value) => env::set_var("GIT_CONFIG_GLOBAL", value),
            None => env::remove_var("GIT_CONFIG_GLOBAL"),
        }

        assert!(
            !sentinel.exists(),
            "ambient global Git alias executed and created {}",
            sentinel.display()
        );
        assert!(
            result.is_err(),
            "hermetic git should ignore the ambient alias and fail unknown subcommand; got {result:?}"
        );
    }

    #[tokio::test]
    async fn git_helper_ignores_env_injected_git_aliases() {
        let _guard = ENV_LOCK.lock().await;

        let temp = TempDir::new().unwrap();
        let sentinel = temp.path().join("env-git-alias-ran");
        let quoted_sentinel = shell_single_quote(&sentinel.display().to_string());
        let previous_count = env::var_os("GIT_CONFIG_COUNT");
        let previous_key = env::var_os("GIT_CONFIG_KEY_0");
        let previous_value = env::var_os("GIT_CONFIG_VALUE_0");

        env::set_var("GIT_CONFIG_COUNT", "1");
        env::set_var("GIT_CONFIG_KEY_0", "alias.owned");
        env::set_var("GIT_CONFIG_VALUE_0", format!("!touch {quoted_sentinel}"));

        let result = git(&["owned"]).await;

        restore_env_var("GIT_CONFIG_VALUE_0", previous_value);
        restore_env_var("GIT_CONFIG_KEY_0", previous_key);
        restore_env_var("GIT_CONFIG_COUNT", previous_count);

        assert!(
            !sentinel.exists(),
            "env-injected Git alias executed and created {}",
            sentinel.display()
        );
        assert!(
            result.is_err(),
            "hermetic git should ignore the env-injected alias and fail unknown subcommand; got {result:?}"
        );
    }

    #[tokio::test]
    async fn hermetic_git_command_removes_repo_template_hook_and_ssh_environment() {
        let _guard = ENV_LOCK.lock().await;

        let dangerous_git_env = [
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_DIR",
            "GIT_EXEC_PATH",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_TEMPLATE_DIR",
            "GIT_WORK_TREE",
        ];
        let previous_values = dangerous_git_env
            .iter()
            .map(|name| (*name, env::var_os(name)))
            .collect::<Vec<_>>();

        for name in dangerous_git_env {
            env::set_var(name, "/tmp/ambient-git-value");
        }

        let mut command = hermetic_git_command();
        let removed_env = command
            .as_std_mut()
            .get_envs()
            .filter_map(|(key, value)| value.is_none().then_some(key.to_owned()))
            .collect::<Vec<_>>();

        for (name, previous) in previous_values {
            restore_env_var(name, previous);
        }

        for name in dangerous_git_env {
            assert!(
                removed_env
                    .iter()
                    .any(|key| key == std::ffi::OsStr::new(name)),
                "hermetic git command must explicitly remove ambient {name}"
            );
        }
    }

    #[tokio::test]
    async fn hermetic_git_command_removes_askpass_helpers_and_disables_terminal_prompts() {
        let _guard = ENV_LOCK.lock().await;

        let temp = TempDir::new().unwrap();
        let sentinel = temp.path().join("ambient-askpass-ran");
        let askpass = temp.path().join("askpass-helper");
        fs::write(
            &askpass,
            format!(
                "#!/bin/sh\ntouch {}\n",
                shell_single_quote(&sentinel.display().to_string())
            ),
        )
        .unwrap();

        let askpass_env = ["GIT_ASKPASS", "SSH_ASKPASS"];
        let previous_values = askpass_env
            .iter()
            .map(|name| (*name, env::var_os(name)))
            .collect::<Vec<_>>();
        let previous_terminal_prompt = env::var_os("GIT_TERMINAL_PROMPT");

        for name in askpass_env {
            env::set_var(name, &askpass);
        }
        env::set_var("GIT_TERMINAL_PROMPT", "1");

        let mut command = hermetic_git_command();
        let env_overrides = command
            .as_std_mut()
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(|value| value.to_owned())))
            .collect::<Vec<_>>();

        restore_env_var("GIT_TERMINAL_PROMPT", previous_terminal_prompt);
        for (name, previous) in previous_values {
            restore_env_var(name, previous);
        }

        for name in askpass_env {
            assert!(
                env_overrides
                    .iter()
                    .any(|(key, value)| key == std::ffi::OsStr::new(name) && value.is_none()),
                "hermetic git command must explicitly remove ambient {name}"
            );
        }
        assert!(
            env_overrides.iter().any(|(key, value)| {
                key == std::ffi::OsStr::new("GIT_TERMINAL_PROMPT")
                    && value.as_deref() == Some(std::ffi::OsStr::new("0"))
            }),
            "hermetic git command must disable interactive credential prompts"
        );
        assert!(
            !sentinel.exists(),
            "ambient askpass helper executed and created {}",
            sentinel.display()
        );
    }

    /// Run hermetic git in `dir` for test fixture setup and return stdout.
    async fn fixture_git(dir: &Path, args: &[&str]) -> String {
        let output = hermetic_git_command()
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Build a bare repository holding one commit with `README.md`, and
    /// return its `file://` URL and the commit SHA.
    async fn bare_repo_with_one_commit(root: &Path) -> (String, String) {
        let src = root.join("src");
        let bare = root.join("bare.git");
        fs::create_dir_all(&src).unwrap();
        fixture_git(&src, &["init", "--quiet"]).await;
        fs::write(src.join("README.md"), "hello from fixture\n").unwrap();
        fixture_git(&src, &["add", "README.md"]).await;
        fixture_git(
            &src,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        )
        .await;
        let sha = fixture_git(&src, &["rev-parse", "HEAD"])
            .await
            .trim()
            .to_string();
        fixture_git(
            root,
            &[
                "clone",
                "--quiet",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .await;
        (format!("file://{}", bare.display()), sha)
    }

    #[tokio::test]
    async fn prepare_workspace_from_clone_url_removes_origin_remote_after_checkout() {
        let fixture = TempDir::new().unwrap();
        let (clone_url, head_sha) = bare_repo_with_one_commit(fixture.path()).await;

        let workspace = prepare_workspace_from_clone_url(&clone_url, &head_sha)
            .await
            .unwrap();

        let remotes = fixture_git(workspace.path(), &["remote"]).await;
        assert_eq!(remotes, "", "workspace must keep no git remote");
        let config = fs::read_to_string(workspace.path().join(".git/config")).unwrap();
        assert!(
            !config.contains("[remote \"origin\"]") && !config.contains("url ="),
            "workspace .git/config must hold no clone URL, got:\n{config}"
        );

        let head = fixture_git(workspace.path(), &["rev-parse", "HEAD"]).await;
        assert_eq!(head.trim(), head_sha);
        assert_eq!(
            fs::read_to_string(workspace.path().join("README.md")).unwrap(),
            "hello from fixture\n"
        );
    }

    /// Commit everything staged in `dir` with a fixed fixture identity.
    async fn fixture_commit(dir: &Path, message: &str) -> String {
        fixture_git(dir, &["add", "-A"]).await;
        fixture_git(
            dir,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        )
        .await;
        fixture_git(dir, &["rev-parse", "HEAD"])
            .await
            .trim()
            .to_string()
    }

    /// SHAs of a pull-request-shaped fixture repository.
    struct DivergedRepo {
        clone_url: String,
        head_sha: String,
        base_sha: String,
        fork_point: String,
        /// `f2`: a feature commit an earlier review covered.
        reviewed_sha: String,
    }

    /// Text long enough for git's rename detection to pair the old and
    /// new paths.
    const RENAMED_BODY: &str = "line one of the renamed file\nline two of the renamed file\n\
line three of the renamed file\nline four of the renamed file\n";

    /// The small edit the pull request makes to the renamed file.
    const RENAMED_ADDED_LINE: &str = "line five, added with the rename";

    /// Bytes git classifies as binary (NUL bytes) with a recognisable
    /// marker that must never appear in a diff.
    const BINARY_BODY: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-BINARY-MARKER\0\0";

    /// Build a bare repository shaped like a pull request whose base
    /// branch moved on after the feature branched:
    ///
    /// ```text
    /// fork_point -- f1 -- f2 -- f3   (feature, head_sha)
    ///            \
    ///             b1 -- b2 -- b3     (main, base_sha)
    /// ```
    ///
    /// The feature edits `README.md`, adds a binary `logo.png`, renames
    /// `old_name.txt` to `new_name.txt` with a one-line addition, and adds
    /// a `.gitattributes` that
    /// marks `notes.txt` (which it also edits) as binary. Main edits
    /// `base_only.txt` after the fork point.
    async fn diverged_repo(root: &Path) -> DivergedRepo {
        let src = root.join("src");
        let bare = root.join("bare.git");
        fs::create_dir_all(&src).unwrap();
        fixture_git(&src, &["init", "--quiet", "--initial-branch=main"]).await;
        fs::write(src.join("README.md"), "hello\n").unwrap();
        fs::write(src.join("old_name.txt"), RENAMED_BODY).unwrap();
        fs::write(src.join("notes.txt"), "original notes\n").unwrap();
        fs::write(src.join("base_only.txt"), "base v0\n").unwrap();
        let fork_point = fixture_commit(&src, "fork point").await;

        fixture_git(&src, &["checkout", "--quiet", "-b", "feature"]).await;
        fs::write(src.join("README.md"), "hello\nfeature line\n").unwrap();
        fixture_commit(&src, "f1").await;
        fs::write(src.join("logo.png"), BINARY_BODY).unwrap();
        fixture_git(&src, &["mv", "old_name.txt", "new_name.txt"]).await;
        fs::write(
            src.join("new_name.txt"),
            format!("{RENAMED_BODY}{RENAMED_ADDED_LINE}\n"),
        )
        .unwrap();
        let reviewed_sha = fixture_commit(&src, "f2").await;
        fs::write(src.join(".gitattributes"), "notes.txt -diff\n").unwrap();
        fs::write(
            src.join("notes.txt"),
            "original notes\nhidden feature change\n",
        )
        .unwrap();
        let head_sha = fixture_commit(&src, "f3").await;

        fixture_git(&src, &["checkout", "--quiet", "main"]).await;
        for n in 1..=3 {
            fs::write(src.join("base_only.txt"), format!("base v{n}\n")).unwrap();
            fixture_commit(&src, &format!("b{n}")).await;
        }
        let base_sha = fixture_git(&src, &["rev-parse", "HEAD"])
            .await
            .trim()
            .to_string();

        fixture_git(
            root,
            &[
                "clone",
                "--quiet",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .await;
        DivergedRepo {
            clone_url: format!("file://{}", bare.display()),
            head_sha,
            base_sha,
            fork_point,
            reviewed_sha,
        }
    }

    #[tokio::test]
    async fn prepare_workspace_with_base_finds_the_merge_base_beyond_the_shallow_clone() {
        let fixture = TempDir::new().unwrap();
        let repo = diverged_repo(fixture.path()).await;

        let workspace = prepare_workspace_from_clone_url_with_base(
            &repo.clone_url,
            &repo.head_sha,
            &repo.base_sha,
        )
        .await
        .unwrap();

        assert_eq!(workspace.merge_base(), Some(repo.fork_point.as_str()));
    }

    /// Prepare a workspace for the [`diverged_repo`] fixture.
    async fn diverged_workspace(root: &Path) -> PreparedWorkspace {
        let repo = diverged_repo(root).await;
        prepare_workspace_from_clone_url_with_base(&repo.clone_url, &repo.head_sha, &repo.base_sha)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pr_diff_compares_head_with_the_merge_base_not_the_base_tip() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;

        let diff = workspace.pr_diff().await.unwrap();

        assert!(
            diff.contains("diff --git a/README.md b/README.md") && diff.contains("+feature line"),
            "diff must hold the feature's change, got:\n{diff}"
        );
        assert!(
            !diff.contains("base_only.txt"),
            "base-branch commits after the fork point must not appear, got:\n{diff}"
        );
    }

    #[tokio::test]
    async fn pr_diff_names_binary_files_without_their_content() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;

        let diff = workspace.pr_diff().await.unwrap();

        assert!(
            diff.contains("Binary files /dev/null and b/logo.png differ"),
            "binary file must be listed as changed, got:\n{diff}"
        );
        assert!(
            !diff.contains("BINARY-MARKER"),
            "binary content must never reach the diff, got:\n{diff}"
        );
    }

    #[tokio::test]
    async fn pr_diff_reports_renames_as_renames() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;

        let diff = workspace.pr_diff().await.unwrap();

        assert!(
            diff.contains("diff --git a/old_name.txt b/new_name.txt")
                && diff.contains("rename from old_name.txt")
                && diff.contains("rename to new_name.txt"),
            "rename must be detected, got:\n{diff}"
        );
    }

    /// Head-side line number of the added line `text` in `path`'s section of
    /// a unified diff, counted from the `+start` of its hunk header the way
    /// the host counts the lines of its own pull-request diff.
    fn head_line_of_added(diff: &str, path: &str, text: &str) -> Option<u32> {
        let header_end = format!(" b/{path}");
        let section = diff.split("diff --git ").find(|section| {
            section
                .lines()
                .next()
                .is_some_and(|header| header.ends_with(&header_end))
        })?;
        let mut line = 0;
        for diff_line in section.lines() {
            if let Some(header) = diff_line.strip_prefix("@@ ") {
                let new_range = header.split(' ').nth(1)?.strip_prefix('+')?;
                line = new_range.split(',').next()?.parse().ok()?;
            } else if let Some(added) = diff_line.strip_prefix('+') {
                if added == text {
                    return Some(line);
                }
                line += 1;
            } else if diff_line.starts_with(' ') {
                line += 1;
            }
        }
        None
    }

    /// Issue #260 C12: a finding on a changed line of the locally diffed
    /// renamed file maps to the new path and the head-side line, exactly as
    /// for the host's diff, and a finding on a file the pull request never
    /// changed (one only the base branch touched after the fork point) is
    /// not posted inline.
    #[tokio::test]
    async fn findings_on_the_local_diff_map_to_the_renamed_path_and_head_line() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;
        let diff = workspace.pr_diff().await.unwrap();
        let line = head_line_of_added(&diff, "new_name.txt", RENAMED_ADDED_LINE)
            .unwrap_or_else(|| panic!("renamed file must carry a hunk, got:\n{diff}"));
        let head_text = fs::read_to_string(workspace.path().join("new_name.txt")).unwrap();
        assert_eq!(
            head_text.lines().nth(line as usize - 1),
            Some(RENAMED_ADDED_LINE)
        );

        let finding = |path: &str, line_start: u32| ar_prompts::ReviewFinding {
            path: path.to_owned(),
            line_start,
            line_end: None,
            severity: ar_prompts::ReviewSeverity::Warning,
            message: format!("finding on {path}"),
            title: String::new(),
            category: ar_prompts::FindingCategory::Correctness,
        };
        let mut output = ar_prompts::ReviewOutput {
            summary: String::new(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: vec![finding("new_name.txt", line), finding("base_only.txt", 1)],
        };
        let changed: Vec<String> = workspace
            .pr_changed_files()
            .await
            .unwrap()
            .into_iter()
            .map(|file| file.filename)
            .collect();
        crate::pipeline::drop_findings_outside_changed_paths(&mut output, &changed);
        let review = crate::mapping::output_to_review_request(&output, &workspace.head_sha);

        let anchors: Vec<_> = review
            .comments
            .iter()
            .map(|c| (c.path.as_str(), c.old_position, c.new_position))
            .collect();
        assert_eq!(anchors, vec![("new_name.txt", None, Some(line))]);
    }

    /// A pull request must not hide its own changes from review by adding
    /// a `.gitattributes` that marks a text file `-diff` (THREAT-MODEL T3a):
    /// attributes come from the merge base, never the head.
    #[tokio::test]
    async fn pr_diff_ignores_gitattributes_added_by_the_pull_request() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;

        let diff = workspace.pr_diff().await.unwrap();

        assert!(
            diff.contains("+hidden feature change"),
            "head-side .gitattributes must not hide a text change, got:\n{diff}"
        );
    }

    fn changed(filename: &str, status: &str, additions: u32, deletions: u32) -> ChangedFile {
        ChangedFile {
            additions,
            changes: additions + deletions,
            deletions,
            filename: filename.to_owned(),
            patch: None,
            status: status.to_owned(),
        }
    }

    #[tokio::test]
    async fn pr_changed_files_lists_every_file_the_pull_request_changes() {
        let fixture = TempDir::new().unwrap();
        let workspace = diverged_workspace(fixture.path()).await;

        let mut files = workspace.pr_changed_files().await.unwrap();
        files.sort_by(|a, b| a.filename.cmp(&b.filename));

        assert_eq!(
            files,
            vec![
                changed(".gitattributes", "added", 1, 0),
                changed("README.md", "modified", 1, 0),
                changed("logo.png", "added", 0, 0),
                changed("new_name.txt", "renamed", 1, 0),
                changed("notes.txt", "modified", 1, 0),
            ]
        );
    }

    #[tokio::test]
    async fn incremental_diff_holds_only_the_commits_after_the_previous_review() {
        let fixture = TempDir::new().unwrap();
        let repo = diverged_repo(fixture.path()).await;
        let workspace = prepare_workspace_from_clone_url_with_previous(
            &repo.clone_url,
            &repo.head_sha,
            &repo.base_sha,
            &repo.reviewed_sha,
        )
        .await
        .unwrap();

        let diff = workspace.incremental_diff().await.unwrap();

        assert!(
            diff.contains("+hidden feature change"),
            "the commit after the reviewed one must be in the diff (attributes from the \
             merge base), got:\n{diff}"
        );
        assert!(
            !diff.contains("README.md") && !diff.contains("logo.png"),
            "commits the previous review covered must not appear, got:\n{diff}"
        );
    }

    #[tokio::test]
    async fn prepare_workspace_with_previous_rejects_non_sha_previous_before_invoking_git() {
        let err = prepare_workspace_from_clone_url_with_previous(
            "https://oauth2:tok@forgejo.example.com/alice/widgets.git",
            "deadbeef",
            "cafef00d",
            "--upload-pack=evil",
        )
        .await
        .expect_err("must reject");

        assert!(
            err.to_string()
                .contains("previous_sha \"--upload-pack=evil\" is not a hex commit SHA"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn prepare_workspace_with_base_rejects_non_sha_base_before_invoking_git() {
        let err = prepare_workspace_from_clone_url_with_base(
            "https://oauth2:tok@forgejo.example.com/alice/widgets.git",
            "deadbeef",
            "--upload-pack=evil",
        )
        .await
        .expect_err("must reject");

        assert!(
            err.to_string()
                .contains("base_sha \"--upload-pack=evil\" is not a hex commit SHA"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn merge_base_deepening_fetches_at_most_the_history_cap() {
        let fetched: u32 = MERGE_BASE_DEEPEN_STEPS.iter().sum();
        assert_eq!(fetched, MERGE_BASE_HISTORY_CAP);
        assert_eq!(MERGE_BASE_HISTORY_CAP, 1000);
    }

    #[tokio::test]
    async fn locate_merge_base_gives_up_when_the_deepen_budget_runs_out() {
        let fixture = TempDir::new().unwrap();
        let repo = diverged_repo(fixture.path()).await;
        let shallow_clone = |name: &'static str| {
            let clone = fixture.path().join(name);
            let repo = &repo;
            async move {
                git(&[
                    OsStr::new("clone"),
                    OsStr::new("--quiet"),
                    OsStr::new("--no-checkout"),
                    OsStr::new("--depth=1"),
                    OsStr::new(&repo.clone_url),
                    clone.as_os_str(),
                ])
                .await
                .unwrap();
                fixture_git(
                    &clone,
                    &["fetch", "--quiet", "--depth=1", "origin", &repo.head_sha],
                )
                .await;
                clone
            }
        };

        // The fork point is three commits behind each tip: one deepening
        // step of one commit cannot reach it, three can.
        let short = shallow_clone("short").await;
        let out_of_budget = locate_merge_base(&short, &repo.head_sha, &repo.base_sha, &[1]).await;
        assert_eq!(out_of_budget, None);
        let long = shallow_clone("long").await;
        let enough = locate_merge_base(&long, &repo.head_sha, &repo.base_sha, &[1, 1, 1]).await;
        assert_eq!(enough, Some(repo.fork_point.clone()));
    }

    #[tokio::test]
    async fn prepare_workspace_with_unrelated_base_keeps_the_workspace_without_a_merge_base() {
        let fixture = TempDir::new().unwrap();
        let src = fixture.path().join("src");
        let bare = fixture.path().join("bare.git");
        fs::create_dir_all(&src).unwrap();
        fixture_git(&src, &["init", "--quiet", "--initial-branch=main"]).await;
        fs::write(src.join("README.md"), "head side\n").unwrap();
        let head_sha = fixture_commit(&src, "head").await;
        fixture_git(&src, &["checkout", "--quiet", "--orphan", "unrelated"]).await;
        fs::write(src.join("README.md"), "unrelated side\n").unwrap();
        let base_sha = fixture_commit(&src, "unrelated").await;
        fixture_git(
            fixture.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .await;
        let clone_url = format!("file://{}", bare.display());

        let workspace =
            prepare_workspace_from_clone_url_with_base(&clone_url, &head_sha, &base_sha)
                .await
                .unwrap();

        assert_eq!(workspace.merge_base(), None);
        assert_eq!(
            fs::read_to_string(workspace.path().join("README.md")).unwrap(),
            "head side\n"
        );
        assert_eq!(fixture_git(workspace.path(), &["remote"]).await, "");
    }

    #[test]
    fn parse_name_status_z_maps_git_codes_to_host_statuses() {
        let stdout = b"A\0new.rs\0D\0gone.rs\0M\0kept.rs\0R087\0old.rs\0moved.rs\0\
C100\0src.rs\0copy.rs\0T\0link\0";

        assert_eq!(
            parse_name_status_z(stdout),
            vec![
                ("added", "new.rs".to_owned()),
                ("removed", "gone.rs".to_owned()),
                ("modified", "kept.rs".to_owned()),
                ("renamed", "moved.rs".to_owned()),
                ("copied", "copy.rs".to_owned()),
                ("changed", "link".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_numstat_z_reads_counts_binary_files_and_renames() {
        let stdout = b"3\t1\tsrc/lib.rs\0-\t-\tlogo.png\x000\t2\t\0old.rs\0moved.rs\0";

        assert_eq!(
            parse_numstat_z(stdout),
            vec![
                ("src/lib.rs".to_owned(), (3, 1)),
                ("logo.png".to_owned(), (0, 0)),
                ("moved.rs".to_owned(), (0, 2)),
            ]
        );
    }

    #[test]
    fn parsers_return_nothing_for_an_empty_diff() {
        assert!(parse_name_status_z(b"").is_empty());
        assert!(parse_numstat_z(b"").is_empty());
    }

    #[tokio::test]
    async fn pr_diff_without_a_merge_base_is_an_error() {
        let fixture = TempDir::new().unwrap();
        let (clone_url, head_sha) = bare_repo_with_one_commit(fixture.path()).await;
        let workspace = prepare_workspace_from_clone_url(&clone_url, &head_sha)
            .await
            .unwrap();

        let err = workspace.pr_diff().await.expect_err("no merge base");

        assert!(
            err.to_string().contains("no merge base"),
            "unexpected error: {err}"
        );
    }

    fn restore_env_var(name: &str, previous: Option<std::ffi::OsString>) {
        match previous {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }

    fn shell_single_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
