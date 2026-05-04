//! Repo-clone helpers for the review pipeline.
//!
//! Linters need files on disk, not just a diff. `prepare_workspace` shallow-
//! clones the target repo into a temp directory, fetches the PR's head SHA,
//! and checks it out. The returned [`PreparedWorkspace`] auto-deletes its
//! tempdir when dropped.

use crate::error::ReviewError;
use std::{env, path::Path};
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

/// Recognise a git SHA: 7–64 hex characters (covers abbreviated
/// SHAs, full SHA-1 = 40 chars, and SHA-256 = 64 chars). Used by
/// `prepare_workspace` to reject argv-injection attempts before
/// the value reaches git.
fn is_valid_git_sha(s: &str) -> bool {
    let len = s.len();
    (7..=64).contains(&len) && s.chars().all(|c| c.is_ascii_hexdigit())
}

async fn git(args: &[&str]) -> Result<(), WorkspaceError> {
    let output = hermetic_git_command().args(args).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(WorkspaceError::Git(redact_token(&stderr)));
    }
    Ok(())
}

async fn git_in(dir: &Path, args: &[&str]) -> Result<(), WorkspaceError> {
    let output = hermetic_git_command()
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
