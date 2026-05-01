//! Implementations of the CLI subcommands.

use crate::cli::{
    InitArgs, ListLintersArgs, RegisterWebhookArgs, ReviewOnceArgs, ValidateConfigArgs,
};
use anyhow::{Context, Result};
use ar_forgejo::{
    Client, CreateAccessTokenRequest, CreateWebhookRequest, InitClient, WebhookConfig,
};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::{run_review_job, InMemoryReviewHistory, ReviewJob};
use ar_prompts::{render_review_prompt, ReviewPromptInputs};
use ar_review::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
use ar_sandbox::DirectSandbox;
use std::sync::Arc;

const WEBHOOK_PATH: &str = "/webhooks/forgejo";

pub async fn init(args: InitArgs) -> Result<()> {
    let password = match args.password {
        Some(p) => p,
        None => rpassword::prompt_password(format!("Password for {}: ", args.username))
            .context("read password")?,
    };

    let client = InitClient::new(&args.forgejo_url, &args.username, &password)
        .context("build init client")?;
    let request = CreateAccessTokenRequest {
        name: args.token_name,
        scopes: args.scopes,
    };
    let token = client
        .create_access_token(&args.username, &request)
        .await
        .context("create access token")?;

    println!("Created access token {}: {}", token.name, token.id);
    println!("Scopes: {}", token.scopes.join(", "));
    println!();
    println!("Save this token (it will not be shown again):");
    println!();
    println!("    export FORGEJO_TOKEN={}", token.sha1);
    println!();
    println!("Recommended next step:");
    println!("    auto_review register-webhook --owner OWNER --repo REPO \\");
    println!("        --forgejo-url {} \\", args.forgejo_url);
    println!("        --gateway-url https://reviewer.example.com \\");
    println!("        --webhook-secret <pick a strong secret>");
    Ok(())
}

/// Run the full review pipeline once against a specific PR. Builds the
/// same Forgejo client + LLM router the gateway uses and invokes
/// orchestrator::run_review_job synchronously (no spawn) so the user can
/// observe the outcome in their terminal.
pub async fn review_once(args: ReviewOnceArgs) -> Result<()> {
    let forgejo =
        Arc::new(Client::new(&args.forgejo_url, &args.token).context("build forgejo client")?);

    let pr = forgejo
        .get_pull_request(&args.owner, &args.repo, args.pr)
        .await
        .context("fetch pull request")?;

    if pr.draft {
        println!("PR #{} is a draft; not reviewing.", pr.number);
        return Ok(());
    }

    if args.dry_run {
        return print_dry_run_prompt(&forgejo, &args, &pr.title, &pr.body).await;
    }

    let provider = Arc::new(
        OpenAiProvider::new(
            &args.llm_base_url,
            args.llm_api_key.as_deref(),
            &args.llm_model,
        )
        .context("build LLM provider")?,
    );
    let llm = Arc::new(LlmRouter::new().with(ModelTier::Reasoning, provider));

    let job = ReviewJob {
        owner: args.owner.clone(),
        repo: args.repo.clone(),
        pr_number: pr.number,
        head_sha: pr.head.sha,
        pr_title: pr.title,
        pr_body: pr.body,
        // review-once is a one-shot debug command: force a full
        // review regardless of any review history that might dedup.
        force: true,
    };

    println!(
        "Reviewing {}/{} #{} at {}",
        args.owner, args.repo, args.pr, job.head_sha
    );
    // Fresh in-memory history each invocation: review-once is a one-
    // shot debug command, so the no-incremental fall-through is what
    // we want.
    let history = InMemoryReviewHistory::new();
    // CLI debug command: no isolation. The user's host already has
    // the linter binaries; that's what they're testing.
    let sandbox = DirectSandbox::new();
    run_review_job(
        &forgejo,
        &llm,
        &args.forgejo_url,
        &args.token,
        &history,
        // review-once is a one-shot debug command — no learnings
        // store wired in. Future: take a path to a SQLite file.
        None,
        &sandbox,
        // No observer either: review-once prints to stdout, doesn't
        // export Prometheus metrics.
        None,
        job,
    )
    .await;
    println!("Done. Check the PR for the posted review.");
    Ok(())
}

async fn print_dry_run_prompt(
    forgejo: &Client,
    args: &ReviewOnceArgs,
    pr_title: &str,
    pr_body: &str,
) -> Result<()> {
    let raw_diff = forgejo
        .get_pr_diff(&args.owner, &args.repo, args.pr)
        .await
        .context("fetch diff")?;
    let diff = cap_diff(&raw_diff, DEFAULT_MAX_DIFF_BYTES);
    let files = forgejo
        .list_changed_files(&args.owner, &args.repo, args.pr)
        .await
        .context("fetch changed files")?;
    let changed_files: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();
    let repo_full = format!("{}/{}", args.owner, args.repo);
    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: &repo_full,
        pr_number: args.pr,
        pr_title,
        pr_body,
        diff: &diff,
        changed_files: &changed_files,
        linter_findings: &[],
        guidelines: "",
        repo_context: "",
    });
    println!("{prompt}");
    Ok(())
}

pub async fn register_webhook(args: RegisterWebhookArgs) -> Result<()> {
    let webhook_url = build_webhook_url(&args.gateway_url);
    let client = Client::new(&args.forgejo_url, &args.token).context("build forgejo client")?;
    let request = CreateWebhookRequest {
        kind: "forgejo".into(),
        config: WebhookConfig {
            url: webhook_url.clone(),
            content_type: "json".into(),
            secret: args.webhook_secret,
        },
        events: vec!["pull_request".into(), "issue_comment".into()],
        active: true,
    };
    let created = client
        .create_webhook(&args.owner, &args.repo, &request)
        .await
        .context("register webhook")?;
    println!(
        "Registered webhook {} on {}/{} → {}",
        created.id, args.owner, args.repo, webhook_url
    );
    Ok(())
}

/// Append `/webhooks/forgejo` to a gateway base URL, normalizing trailing
/// slashes.
pub fn build_webhook_url(gateway_url: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    format!("{trimmed}{WEBHOOK_PATH}")
}

/// Print the bundled linter catalogue. With `--json`, emits one
/// JSON object per line (newline-delimited JSON). Otherwise renders
/// a human-readable table grouped by language.
pub fn list_linters(args: ListLintersArgs) -> Result<()> {
    let mut entries: Vec<&ar_tools::LinterInfo> = ar_tools::linter_catalogue().iter().collect();
    if let Some(lang) = args.language.as_deref() {
        let lang = lang.to_ascii_lowercase();
        entries.retain(|e| e.languages.iter().any(|l| l.eq_ignore_ascii_case(&lang)));
        if entries.is_empty() {
            anyhow::bail!(
                "no linters tagged with language `{lang}`. Run `auto_review list-linters` (no filter) to see all tags."
            );
        }
    }
    if args.json {
        for entry in &entries {
            println!("{}", serde_json::to_string(entry)?);
        }
        return Ok(());
    }
    // Human-readable: column-aligned name + description.
    let widest = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    println!(
        "{} bundled linter{}{}",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        match args.language.as_deref() {
            Some(l) => format!(" tagged `{l}`"),
            None => String::new(),
        }
    );
    println!();
    for entry in &entries {
        println!(
            "  {:width$}  {}",
            entry.name,
            entry.description,
            width = widest
        );
        println!(
            "  {:width$}  languages: {}",
            "",
            entry.languages.join(", "),
            width = widest
        );
        println!("  {:width$}  {}", "", entry.homepage, width = widest);
        println!();
    }
    println!(
        "Use any of these names under `disabled_tools:` in .auto_review.yaml to skip a linter."
    );
    Ok(())
}

/// Validate one or more `.auto_review.yaml` files. Each path can be a
/// file or a directory; directories are scanned for the standard
/// config filenames. Returns Ok with the count of validated files;
/// returns Err when any file fails to parse or no files are found.
pub fn validate_config(args: ValidateConfigArgs) -> Result<()> {
    let files = expand_config_paths(&args.paths)?;
    if files.is_empty() {
        anyhow::bail!("no .auto_review.yaml files found at the supplied paths");
    }
    let mut failures: Vec<(std::path::PathBuf, String)> = Vec::new();
    for file in &files {
        let body = match std::fs::read_to_string(file) {
            Ok(b) => b,
            Err(e) => {
                failures.push((file.clone(), format!("read: {e}")));
                continue;
            }
        };
        match ar_review::parse_repo_config(&body) {
            Ok(cfg) => {
                println!(
                    "✓ {}: enabled={}, ignored={}, disabled_tools={}",
                    file.display(),
                    cfg.enabled,
                    cfg.ignored_paths.len(),
                    cfg.disabled_tools.len()
                );
            }
            Err(e) => {
                let detail = if let Some(loc) = e.location() {
                    format!("line {}, column {}: {e}", loc.line(), loc.column())
                } else {
                    e.to_string()
                };
                failures.push((file.clone(), detail));
            }
        }
    }
    for (path, detail) in &failures {
        eprintln!("✗ {}: {}", path.display(), detail);
    }
    if failures.is_empty() {
        println!("validated {} file(s)", files.len());
        Ok(())
    } else {
        anyhow::bail!(
            "{} of {} file(s) failed validation",
            failures.len(),
            files.len()
        );
    }
}

/// Resolve each input path: a file is taken as-is; a directory is
/// scanned for `.auto_review.yaml` and `.auto_review.yml`. Sorted
/// + deduplicated so output ordering is stable.
fn expand_config_paths(paths: &[std::path::PathBuf]) -> Result<Vec<std::path::PathBuf>> {
    use std::collections::BTreeSet;
    let mut out: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for p in paths {
        let meta = std::fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
        if meta.is_dir() {
            for name in [".auto_review.yaml", ".auto_review.yml"] {
                let candidate = p.join(name);
                if candidate.exists() {
                    out.insert(candidate);
                }
            }
        } else {
            out.insert(p.clone());
        }
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_linters_no_filter_succeeds() {
        let args = ListLintersArgs {
            language: None,
            json: false,
        };
        list_linters(args).expect("default catalogue print");
    }

    #[test]
    fn list_linters_with_known_language_succeeds() {
        let args = ListLintersArgs {
            language: Some("python".into()),
            json: true,
        };
        list_linters(args).expect("python filter");
    }

    #[test]
    fn list_linters_with_unknown_language_errors() {
        let args = ListLintersArgs {
            language: Some("klingon".into()),
            json: false,
        };
        let err = list_linters(args).expect_err("unknown language should fail");
        assert!(err.to_string().contains("klingon"));
    }

    #[test]
    fn validate_config_succeeds_on_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".auto_review.yaml"),
            "enabled: true\nignored_paths:\n  - vendor/**\n",
        )
        .unwrap();
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
        };
        validate_config(args).expect("valid config");
    }

    #[test]
    fn validate_config_fails_on_malformed_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".auto_review.yaml"),
            "enabled: not_a_bool\n",
        )
        .unwrap();
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
        };
        let err = validate_config(args).expect_err("malformed should fail");
        assert!(err.to_string().contains("failed validation"));
    }

    #[test]
    fn validate_config_fails_when_no_files_found() {
        let dir = tempfile::tempdir().unwrap();
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
        };
        let err = validate_config(args).expect_err("empty dir should fail");
        assert!(err.to_string().contains("no .auto_review.yaml"));
    }

    #[test]
    fn webhook_url_appends_path() {
        assert_eq!(
            build_webhook_url("https://reviewer.example.com"),
            "https://reviewer.example.com/webhooks/forgejo"
        );
    }

    #[test]
    fn webhook_url_handles_trailing_slash() {
        assert_eq!(
            build_webhook_url("https://reviewer.example.com/"),
            "https://reviewer.example.com/webhooks/forgejo"
        );
    }

    #[test]
    fn webhook_url_handles_subpath() {
        assert_eq!(
            build_webhook_url("https://x.example/auto/"),
            "https://x.example/auto/webhooks/forgejo"
        );
    }
}
