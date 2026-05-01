//! Repo-clone helpers for the review pipeline.
//!
//! Linters need files on disk, not just a diff. `prepare_workspace` shallow-
//! clones the target repo into a temp directory, fetches the PR's head SHA,
//! and checks it out. The returned [`PreparedWorkspace`] auto-deletes its
//! tempdir when dropped.

use crate::error::ReviewError;
use std::path::Path;
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
pub struct PreparedWorkspace {
    dir: TempDir,
}

impl PreparedWorkspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
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
/// SHA isn't on the default branch.
pub async fn prepare_workspace(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    head_sha: &str,
) -> Result<PreparedWorkspace, WorkspaceError> {
    let url = build_clone_url(base, owner, repo, token)?;
    let dir = TempDir::new()?;
    let path = dir.path().to_owned();

    git(&[
        "clone",
        "--no-checkout",
        "--depth=1",
        &url,
        path.to_str().expect("tempdir path is utf-8"),
    ])
    .await?;
    git_in(&path, &["fetch", "--depth=1", "origin", head_sha]).await?;
    git_in(&path, &["checkout", head_sha]).await?;

    Ok(PreparedWorkspace { dir })
}

async fn git(args: &[&str]) -> Result<(), WorkspaceError> {
    let output = Command::new("git").args(args).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(WorkspaceError::Git(redact_token(&stderr)));
    }
    Ok(())
}

async fn git_in(dir: &Path, args: &[&str]) -> Result<(), WorkspaceError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(WorkspaceError::Git(redact_token(&stderr)));
    }
    Ok(())
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
}
