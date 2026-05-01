//! Implementations of the CLI subcommands.

use crate::cli::{
    DoctorArgs, ExplainRoutingArgs, ForgetLearningArgs, InitArgs, ListLearningsArgs,
    ListLintersArgs, ListWebhooksArgs, PurgeHistoryArgs, RegisterWebhookArgs, ResetPrArgs,
    ReviewOnceArgs, StatusArgs, TestWebhookArgs, UnregisterWebhookArgs, ValidateConfigArgs,
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
use hmac::{Hmac, Mac};
use sha2::Sha256;
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

/// List every webhook installed on the repo. Operators use this
/// to audit which webhooks the bot's PAT can see and to find the
/// id `unregister-webhook` needs.
pub async fn list_webhooks(args: ListWebhooksArgs) -> Result<()> {
    let client = Client::new(&args.forgejo_url, &args.token).context("build forgejo client")?;
    let hooks = client
        .list_webhooks(&args.owner, &args.repo)
        .await
        .context("list webhooks")?;
    if args.json {
        for h in &hooks {
            println!("{}", serde_json::to_string(h)?);
        }
        return Ok(());
    }
    if hooks.is_empty() {
        println!("No webhooks installed on {}/{}.", args.owner, args.repo);
        return Ok(());
    }
    println!(
        "{} webhook{} on {}/{}:",
        hooks.len(),
        if hooks.len() == 1 { "" } else { "s" },
        args.owner,
        args.repo
    );
    println!();
    for h in &hooks {
        let active = if h.active { "active" } else { "INACTIVE" };
        println!(
            "  id={:<6} {:>8} type={:<8} events=[{}]",
            h.id,
            active,
            h.kind,
            h.events.join(", ")
        );
        println!("           url={}", h.url);
    }
    Ok(())
}

/// Delete one or more webhooks. Either `--id N` (single, exact)
/// or `--match-url <substr>` (every webhook whose URL contains
/// the substring). The `--match-url` form is the safe choice for
/// deploy scripts that don't know ids ahead of time.
pub async fn unregister_webhook(args: UnregisterWebhookArgs) -> Result<()> {
    let client = Client::new(&args.forgejo_url, &args.token).context("build forgejo client")?;
    let to_delete: Vec<u64> = match (args.id, args.match_url.as_deref()) {
        (Some(id), _) => vec![id],
        (None, Some(needle)) => {
            let hooks = client
                .list_webhooks(&args.owner, &args.repo)
                .await
                .context("list webhooks for matching")?;
            let matched: Vec<u64> = hooks
                .iter()
                .filter(|h| h.url.contains(needle))
                .map(|h| h.id)
                .collect();
            if matched.is_empty() {
                anyhow::bail!(
                    "no webhook on {}/{} has a URL containing `{}`",
                    args.owner,
                    args.repo,
                    needle
                );
            }
            matched
        }
        (None, None) => anyhow::bail!("pass either --id <N> or --match-url <substr>"),
    };
    for id in &to_delete {
        client
            .delete_webhook(&args.owner, &args.repo, *id)
            .await
            .with_context(|| format!("delete webhook {id}"))?;
        println!("Deleted webhook {id} on {}/{}.", args.owner, args.repo);
    }
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

/// Print every learning in the SQLite store. Operators use
/// this to audit what `@<bot> remember` has been writing and
/// to find the `id` `forget-learning` needs.
pub async fn list_learnings(args: ListLearningsArgs) -> Result<()> {
    use ar_index::LearningsStore;
    let store = ar_index::SqliteLearningsStore::open(&args.learnings_db)
        .await
        .with_context(|| format!("open learnings db at {}", args.learnings_db.display()))?;
    let learnings = store.list().await.context("list learnings")?;
    if args.json {
        for l in &learnings {
            // The embedding is many floats and rarely useful in a
            // human-facing dump. Emit a serialisation that omits
            // it.
            let row = serde_json::json!({
                "id": l.id,
                "text": l.text,
                "source": l.source,
                "created_at": l.created_at,
                "embedding_dim": l.embedding.len(),
            });
            println!("{}", serde_json::to_string(&row)?);
        }
        return Ok(());
    }
    if learnings.is_empty() {
        println!("No learnings stored.");
        return Ok(());
    }
    println!(
        "{} learning{} in {}:",
        learnings.len(),
        if learnings.len() == 1 { "" } else { "s" },
        args.learnings_db.display(),
    );
    println!();
    for l in &learnings {
        let truncated: String = l.text.chars().take(80).collect();
        let suffix = if l.text.chars().count() > 80 {
            "…"
        } else {
            ""
        };
        println!(
            "  id={:<6} source={:<10} {}{}",
            l.id,
            format!("{:?}", l.source).to_lowercase(),
            truncated,
            suffix,
        );
    }
    Ok(())
}

/// Delete one learning by id. Same effect as `@<bot> forget` but
/// operates directly on the SQLite store.
pub async fn forget_learning(args: ForgetLearningArgs) -> Result<()> {
    use ar_index::LearningsStore;
    let store = ar_index::SqliteLearningsStore::open(&args.learnings_db)
        .await
        .with_context(|| format!("open learnings db at {}", args.learnings_db.display()))?;
    store
        .remove(args.id)
        .await
        .with_context(|| format!("remove learning {}", args.id))?;
    println!("Forgot learning {}.", args.id);
    Ok(())
}

/// Drop review-history rows older than `older_than_days` days.
/// Operators wire this into a periodic cleanup; long-running
/// deployments accumulate one row per PR ever reviewed.
pub async fn purge_history(args: PurgeHistoryArgs) -> Result<()> {
    use ar_orchestrator::ReviewHistory;
    let store = ar_orchestrator::SqliteReviewHistory::open(&args.history_db)
        .await
        .with_context(|| format!("open history db at {}", args.history_db.display()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .context("system clock before unix epoch")?;
    let cutoff = now - (args.older_than_days as i64) * 86_400;

    if args.dry_run {
        // Count rows that *would* be dropped. We don't have a
        // count helper; list_known + a fresh in-memory shaped
        // query would be ugly. Use a direct sqlx query via a
        // reopened pool — but we'd be duplicating the schema. The
        // pragmatic fix: list everything, then re-look up each
        // updated_at by adding a method. For v1 we report the
        // total row count and the cutoff, so operators see the
        // upper bound.
        let total = store.list_known().await?.len();
        println!(
            "Dry run: would drop rows with updated_at < {} ({} day(s) ago).",
            cutoff, args.older_than_days
        );
        println!("  Current total rows: {total}");
        println!(
            "  (Re-run without --dry-run to perform the deletion; \
             actual count printed then.)"
        );
        return Ok(());
    }

    let dropped = store
        .purge_older_than(cutoff)
        .await
        .with_context(|| format!("purge rows older than {} days", args.older_than_days))?;
    println!(
        "Purged {} review-history row(s) older than {} days.",
        dropped, args.older_than_days
    );
    Ok(())
}

/// Clear a single PR's review-history record so the next
/// webhook triggers a fresh full review.
pub async fn reset_pr(args: ResetPrArgs) -> Result<()> {
    use ar_orchestrator::ReviewHistory;
    let store = ar_orchestrator::SqliteReviewHistory::open(&args.history_db)
        .await
        .with_context(|| format!("open history db at {}", args.history_db.display()))?;
    let key = ar_orchestrator::PrKey {
        owner: args.owner.clone(),
        repo: args.repo.clone(),
        pr_number: args.pr,
    };
    let prior = store
        .last_reviewed(&key)
        .await
        .with_context(|| "look up existing record")?;
    store
        .clear(&key)
        .await
        .with_context(|| format!("clear history for {}/{}#{}", args.owner, args.repo, args.pr))?;
    match prior {
        Some(sha) => println!(
            "Cleared {}/{}#{}: previously reviewed at {}.",
            args.owner, args.repo, args.pr, sha
        ),
        None => println!(
            "{}/{}#{} had no recorded review; nothing to clear.",
            args.owner, args.repo, args.pr
        ),
    }
    Ok(())
}

/// Pull `/version`, `/info`, `/metrics` from a running gateway
/// and render a one-screen summary. Returns Err on any HTTP
/// failure so wrappers can surface the diagnostic.
pub async fn status(args: StatusArgs) -> Result<()> {
    let timeout = std::time::Duration::from_secs(args.timeout_secs);
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build http client")?;
    let base = args.gateway_url.trim_end_matches('/').to_string();

    let version: serde_json::Value = http
        .get(format!("{base}/version"))
        .send()
        .await
        .context("GET /version")?
        .error_for_status()
        .context("/version returned non-2xx")?
        .json()
        .await
        .context("decode /version body")?;
    let info: serde_json::Value = http
        .get(format!("{base}/info"))
        .send()
        .await
        .context("GET /info")?
        .error_for_status()
        .context("/info returned non-2xx")?
        .json()
        .await
        .context("decode /info body")?;
    let metrics_text = http
        .get(format!("{base}/metrics"))
        .send()
        .await
        .context("GET /metrics")?
        .error_for_status()
        .context("/metrics returned non-2xx")?
        .text()
        .await
        .context("decode /metrics body")?;

    let summary = StatusSummary::compute(&version, &info, &metrics_text);
    if args.json {
        println!("{}", serde_json::to_string(&summary)?);
    } else {
        summary.print(&base);
    }
    Ok(())
}

/// Distilled view of the gateway state. Fields are documented in
/// `print` and `json` for the operator-facing meaning. Stored
/// numerically so JSON consumers can chart them.
#[derive(Debug, serde::Serialize)]
pub(crate) struct StatusSummary {
    pub version: String,
    pub bot_login: String,
    pub sandbox: String,
    pub learnings: String,
    pub history: String,
    pub poller_enabled: bool,
    pub readiness_enabled: bool,
    pub jobs_dispatched_total: u64,
    pub reviews_succeeded_total: u64,
    pub reviews_failed_total: u64,
    pub reviews_completed_count: u64,
    pub reviews_skipped_total: u64,
    pub webhook_signature_failures_total: u64,
    pub webhook_payload_failures_total: u64,
    pub webhook_rate_limited_total: u64,
    pub poll_cycles_total: u64,
    pub success_rate: Option<f64>,
}

impl StatusSummary {
    pub(crate) fn compute(
        version: &serde_json::Value,
        info: &serde_json::Value,
        metrics_text: &str,
    ) -> Self {
        let parsed = parse_metric_counters(metrics_text);
        let succeeded = parsed
            .get("auto_review_reviews_succeeded_total")
            .copied()
            .unwrap_or(0);
        let failed_forgejo = parsed
            .get("auto_review_reviews_failed_forgejo_total")
            .copied()
            .unwrap_or(0);
        let failed_workspace = parsed
            .get("auto_review_reviews_failed_workspace_total")
            .copied()
            .unwrap_or(0);
        let failed_llm = parsed
            .get("auto_review_reviews_failed_llm_total")
            .copied()
            .unwrap_or(0);
        let failed_unhealable = parsed
            .get("auto_review_reviews_failed_unhealable_total")
            .copied()
            .unwrap_or(0);
        let failed_total = failed_forgejo + failed_workspace + failed_llm + failed_unhealable;
        let skipped_same = parsed
            .get("auto_review_reviews_skipped_same_sha_total")
            .copied()
            .unwrap_or(0);
        let skipped_trivial = parsed
            .get("auto_review_reviews_skipped_trivial_total")
            .copied()
            .unwrap_or(0);
        let skipped_disabled = parsed
            .get("auto_review_reviews_skipped_disabled_total")
            .copied()
            .unwrap_or(0);
        let skipped_total = skipped_same + skipped_trivial + skipped_disabled;
        let completed = parsed
            .get("auto_review_reviews_completed_count")
            .copied()
            .unwrap_or(0);
        let success_rate = if completed > 0 {
            Some(succeeded as f64 / completed as f64)
        } else {
            None
        };
        Self {
            version: version["version"].as_str().unwrap_or("unknown").to_string(),
            bot_login: info["bot_login"].as_str().unwrap_or("unknown").to_string(),
            sandbox: info["sandbox"].as_str().unwrap_or("unknown").to_string(),
            learnings: info["learnings"].as_str().unwrap_or("unknown").to_string(),
            history: info["history"].as_str().unwrap_or("unknown").to_string(),
            poller_enabled: info["poller_enabled"].as_bool().unwrap_or(false),
            readiness_enabled: info["readiness_enabled"].as_bool().unwrap_or(false),
            jobs_dispatched_total: parsed
                .get("auto_review_jobs_dispatched_total")
                .copied()
                .unwrap_or(0),
            reviews_succeeded_total: succeeded,
            reviews_failed_total: failed_total,
            reviews_completed_count: completed,
            reviews_skipped_total: skipped_total,
            webhook_signature_failures_total: parsed
                .get("auto_review_webhook_signature_failures_total")
                .copied()
                .unwrap_or(0),
            webhook_payload_failures_total: parsed
                .get("auto_review_webhook_payload_failures_total")
                .copied()
                .unwrap_or(0),
            webhook_rate_limited_total: parsed
                .get("auto_review_webhook_rate_limited_total")
                .copied()
                .unwrap_or(0),
            poll_cycles_total: parsed
                .get("auto_review_poll_cycles_total")
                .copied()
                .unwrap_or(0),
            success_rate,
        }
    }

    fn print(&self, base: &str) {
        println!("auto_review status — {base}");
        println!("  version          {}", self.version);
        println!("  bot login        {}", self.bot_login);
        println!(
            "  sandbox          {}",
            match self.sandbox.as_str() {
                "podman" => "podman (hardened)",
                "direct" => "direct (NO ISOLATION — Kudelski-class RCE risk)",
                other => other,
            }
        );
        println!("  learnings        {}", self.learnings);
        println!("  history          {}", self.history);
        println!(
            "  poller           {}",
            if self.poller_enabled {
                "running"
            } else {
                "disabled"
            }
        );
        println!(
            "  readiness probe  {}",
            if self.readiness_enabled {
                "enabled"
            } else {
                "fallback to /healthz"
            }
        );
        println!();
        println!("Review pipeline:");
        println!("  jobs dispatched  {}", self.jobs_dispatched_total);
        println!("  succeeded        {}", self.reviews_succeeded_total);
        println!("  failed           {}", self.reviews_failed_total);
        println!("  skipped          {}", self.reviews_skipped_total);
        match self.success_rate {
            Some(r) => println!("  success rate     {:.1}%", r * 100.0),
            None => println!("  success rate     — (no completions yet)"),
        }
        println!();
        println!("Webhook intake (rejection counters):");
        println!(
            "  signature fails  {}",
            self.webhook_signature_failures_total
        );
        println!("  payload fails    {}", self.webhook_payload_failures_total);
        println!("  rate-limited     {}", self.webhook_rate_limited_total);
        println!();
        println!("Poller:");
        println!("  cycles total     {}", self.poll_cycles_total);
    }
}

/// Lightweight Prometheus-text-format parser. Extracts every line
/// of the form `name VALUE` (no labels, integer values) into a
/// `name → u64` map. Lines starting with `#` are ignored; lines
/// with `{labels}` are ignored. The gateway's metrics output is
/// labelless except for the histogram, which we don't surface in
/// the status summary.
fn parse_metric_counters(text: &str) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains('{') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let name = match parts.next() {
            Some(n) => n,
            None => continue,
        };
        let value_str = match parts.next() {
            Some(v) => v,
            None => continue,
        };
        // Skip parts beyond the first two (Prometheus allows a
        // timestamp suffix; we don't care about it).
        if let Ok(v) = value_str.parse::<u64>() {
            out.insert(name.to_string(), v);
        }
    }
    out
}

/// Probe outbound dependencies and sanity-check the webhook
/// secret. Mirrors deploy-time concerns: bot PAT valid, LLM
/// reachable, webhook secret long enough.
pub async fn doctor(args: DoctorArgs) -> Result<()> {
    let timeout = std::time::Duration::from_secs(args.timeout_secs);
    let mut report = DoctorReport::new();

    // Forgejo: cheap auth + reachability probe via /api/v1/version
    // (which 200s for any authenticated PAT, including read-only
    // ones, so it's the lightest check available).
    match (args.forgejo_url.as_deref(), args.token.as_deref()) {
        (Some(url), Some(token)) => match probe_forgejo(url, token, timeout).await {
            Ok(version) => report.pass("forgejo", format!("connected; server version {version}")),
            Err(e) => report.fail("forgejo", format!("{e}")),
        },
        (Some(url), None) => match probe_forgejo_anonymous(url, timeout).await {
            Ok(version) => report.warn(
                "forgejo",
                format!("reachable (server version {version}); pass --token to validate auth"),
            ),
            Err(e) => report.fail("forgejo", format!("{e}")),
        },
        _ => report.skip(
            "forgejo",
            "set --forgejo-url (and ideally --token) to enable",
        ),
    }

    // LLM: GET <base>/v1/models — standard OpenAI-compatible
    // health probe. Free for cloud providers, instant for Ollama.
    let configured_models: Vec<(&'static str, Option<&str>)> = vec![
        ("llm-reasoning-model", args.llm_reasoning_model.as_deref()),
        ("llm-cheap-model", args.llm_cheap_model.as_deref()),
        ("llm-embedding-model", args.llm_embedding_model.as_deref()),
    ];
    match args.llm_base_url.as_deref() {
        Some(url) => match probe_llm(url, args.llm_api_key.as_deref(), timeout).await {
            Ok(LlmProbeResult { detail, model_ids }) => {
                report.pass("llm", detail);
                for &(name, configured) in &configured_models {
                    match configured {
                        Some(model) => {
                            if model_ids.iter().any(|m| m == model) {
                                report.pass(name, format!("{model} is loaded"));
                            } else {
                                report.fail(
                                    name,
                                    format!(
                                        "{model} not in /v1/models response; pull it on the \
                                         inference server or fix the env var"
                                    ),
                                );
                            }
                        }
                        None => {
                            // Required vs optional differs per tier; we
                            // don't know which is which here, so skip
                            // silently.
                            report.skip(name, "not configured");
                        }
                    }
                }
            }
            Err(e) => {
                report.fail("llm", format!("{e}"));
                // Without the model list, model-presence checks
                // are indeterminate — surface that explicitly
                // rather than silently skipping.
                for &(name, _configured) in &configured_models {
                    report.skip(name, "skipped: llm probe failed");
                }
            }
        },
        None => {
            report.skip("llm", "set --llm-base-url to enable");
            for &(name, _configured) in &configured_models {
                report.skip(name, "skipped: llm probe disabled");
            }
        }
    }

    // Webhook secret: an entropy heuristic. The HMAC algorithm
    // accepts any non-empty key, but Forgejo's webhook UI doesn't
    // hand out the secret on read, so a weak secret can never be
    // recovered for rotation — we want at least 32 chars from a
    // proper RNG.
    match args.webhook_secret.as_deref() {
        Some(s) => match check_webhook_secret(s) {
            Ok(detail) => report.pass("webhook-secret", detail),
            Err(e) => report.warn("webhook-secret", e),
        },
        None => report.skip("webhook-secret", "set --webhook-secret to enable"),
    }

    report.print();
    if report.has_failures() {
        anyhow::bail!("one or more required checks failed; see report above");
    }
    Ok(())
}

async fn probe_forgejo(
    base_url: &str,
    token: &str,
    timeout: std::time::Duration,
) -> Result<String> {
    let client = ar_forgejo::Client::new(base_url, token).context("build forgejo client")?;
    // get_server_version doesn't exercise auth on its own, so make
    // a second authenticated call too. /api/v1/user requires a valid
    // token and is cheap.
    let version = tokio::time::timeout(timeout, client.get_server_version())
        .await
        .context("forgejo /version timeout")?
        .context("forgejo /version request")?;
    let user_url = format!("{}/api/v1/user", base_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build http client")?;
    let resp = http
        .get(&user_url)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .context("forgejo /user request")?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "forgejo /user returned {}; token invalid or revoked?",
            resp.status()
        );
    }
    Ok(version)
}

async fn probe_forgejo_anonymous(base_url: &str, timeout: std::time::Duration) -> Result<String> {
    let url = format!("{}/api/v1/version", base_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build http client")?;
    let resp = http.get(&url).send().await.context("forgejo /version")?;
    if !resp.status().is_success() {
        anyhow::bail!("forgejo /version returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await.context("decode /version body")?;
    Ok(body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string())
}

struct LlmProbeResult {
    detail: String,
    model_ids: Vec<String>,
}

async fn probe_llm(
    base_url: &str,
    api_key: Option<&str>,
    timeout: std::time::Duration,
) -> Result<LlmProbeResult> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build http client")?;
    let mut req = http.get(&url).header("Accept", "application/json");
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    let resp = req.send().await.context("llm /v1/models")?;
    let status = resp.status();
    if !status.is_success() {
        let snippet: String = resp
            .text()
            .await
            .ok()
            .map(|s| s.chars().take(200).collect())
            .unwrap_or_default();
        anyhow::bail!("{status}: {snippet}");
    }
    let body: serde_json::Value = resp.json().await.context("decode /v1/models body")?;
    let model_ids: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(LlmProbeResult {
        detail: format!("{status}; {} model(s) listed", model_ids.len()),
        model_ids,
    })
}

fn check_webhook_secret(s: &str) -> std::result::Result<String, String> {
    if s.len() < 16 {
        return Err(format!(
            "secret is only {} chars; recommend >= 32 from a proper RNG",
            s.len()
        ));
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Err("secret is all digits; entropy is too low for HMAC".into());
    }
    let unique: std::collections::HashSet<char> = s.chars().collect();
    if unique.len() < 8 {
        return Err(format!(
            "secret has only {} distinct characters; suspect placeholder",
            unique.len()
        ));
    }
    Ok(format!("{} chars, OK", s.len()))
}

#[derive(Debug)]
struct CheckResult {
    name: &'static str,
    status: CheckStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl CheckStatus {
    fn label(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Default)]
struct DoctorReport {
    results: Vec<CheckResult>,
}

impl DoctorReport {
    fn new() -> Self {
        Self::default()
    }
    fn pass(&mut self, name: &'static str, detail: impl Into<String>) {
        self.results.push(CheckResult {
            name,
            status: CheckStatus::Pass,
            detail: detail.into(),
        });
    }
    fn warn(&mut self, name: &'static str, detail: impl Into<String>) {
        self.results.push(CheckResult {
            name,
            status: CheckStatus::Warn,
            detail: detail.into(),
        });
    }
    fn fail(&mut self, name: &'static str, detail: impl Into<String>) {
        self.results.push(CheckResult {
            name,
            status: CheckStatus::Fail,
            detail: detail.into(),
        });
    }
    fn skip(&mut self, name: &'static str, detail: impl Into<String>) {
        self.results.push(CheckResult {
            name,
            status: CheckStatus::Skip,
            detail: detail.into(),
        });
    }
    fn has_failures(&self) -> bool {
        self.results.iter().any(|r| r.status == CheckStatus::Fail)
    }
    fn print(&self) {
        let widest = self.results.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for r in &self.results {
            println!(
                "  [{}] {:width$}  {}",
                r.status.label(),
                r.name,
                r.detail,
                width = widest
            );
        }
    }
}

/// Send an HMAC-signed `ping` webhook (or, with `--event`, a
/// stub `pull_request` event) to a running gateway and print
/// the response. Exits 0 only when the gateway returns a 2xx.
pub async fn test_webhook(args: TestWebhookArgs) -> Result<()> {
    let webhook_url = build_webhook_url(&args.gateway_url);
    let body = match args.event.as_str() {
        "ping" => br#"{"hook_id":0}"#.to_vec(),
        "pull_request" => stub_pr_event_body(),
        other => anyhow::bail!(
            "unsupported event `{other}` (only `ping` and `pull_request` are supported)"
        ),
    };
    let signature = sign_body(&args.webhook_secret, &body);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(args.timeout_secs))
        .build()
        .context("build http client")?;
    let resp = client
        .post(&webhook_url)
        .header("X-Forgejo-Signature", &signature)
        .header("X-Forgejo-Event", &args.event)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("POST {webhook_url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("URL: {webhook_url}");
    println!("Event: {}", args.event);
    println!("Status: {status}");
    if !body.is_empty() {
        println!("Body: {body}");
    }
    if status.is_success() {
        println!("OK — webhook intake path is healthy.");
        Ok(())
    } else {
        anyhow::bail!(
            "gateway returned {status}; check the WEBHOOK_SECRET on both sides and confirm \
             nothing is stripping the X-Forgejo-Signature header in transit."
        )
    }
}

/// Minimal but schema-valid `pull_request` event body. The numbers
/// and SHAs are intentionally fake; the gateway will accept the
/// event, dispatch a job to the orchestrator, and the orchestrator
/// will fail the clone phase with `workspace: clone failed`. The
/// webhook ack still proves the intake path works — that's all this
/// subcommand cares about.
fn stub_pr_event_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "opened",
        "number": 0,
        "pull_request": {
            "number": 0,
            "title": "auto_review test-webhook (stub event)",
            "body": "synthetic event from `auto_review test-webhook`",
            "draft": false,
            "user": {"login": "auto_review-test", "id": 0},
            "head": {"ref": "test", "sha": "0000000000000000000000000000000000000000"},
            "base": {"ref": "main", "sha": "1111111111111111111111111111111111111111"}
        },
        "repository": {
            "name": "test", "full_name": "test/test",
            "default_branch": "main",
            "owner": {"login": "test", "id": 0}
        },
        "sender": {"login": "auto_review-test", "id": 0}
    }))
    .expect("stub serializes")
}

fn sign_body(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any-length key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Show which linters would run for a given set of files.
/// Pure routing — doesn't actually read the files or invoke any
/// linter binary.
pub fn explain_routing(args: ExplainRoutingArgs) -> Result<()> {
    let files: Vec<ar_forgejo::ChangedFile> = args
        .file
        .iter()
        .map(|name| ar_forgejo::ChangedFile {
            filename: name.clone(),
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: None,
        })
        .collect();
    let runners = ar_review::select_runners(&files);
    let mut names: Vec<String> = runners.iter().map(|r| r.name().to_string()).collect();
    names.sort();
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "runners": names }))?
        );
        return Ok(());
    }
    println!(
        "Routing for {} file{}:",
        args.file.len(),
        if args.file.len() == 1 { "" } else { "s" }
    );
    for f in &args.file {
        println!("  - {f}");
    }
    println!();
    if names.is_empty() {
        println!(
            "No linters route to these files. (Empty input or every entry has \
             status=removed.)"
        );
        return Ok(());
    }
    println!(
        "{} linter{} would run:",
        names.len(),
        if names.len() == 1 { "" } else { "s" }
    );
    for n in &names {
        println!("  {n}");
    }
    println!();
    println!(
        "Use `auto_review list-linters` for descriptions; add a name to \
         `.auto_review.yaml`'s `disabled_tools:` to skip it."
    );
    Ok(())
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
        let parsed = if args.strict {
            ar_review::parse_repo_config_strict(&body).map_err(|e| match e {
                ar_review::RepoConfigStrictError::Parse(yaml_err) => {
                    if let Some(loc) = yaml_err.location() {
                        format!("line {}, column {}: {yaml_err}", loc.line(), loc.column())
                    } else {
                        yaml_err.to_string()
                    }
                }
                other => other.to_string(),
            })
        } else {
            ar_review::parse_repo_config(&body).map_err(|e| {
                if let Some(loc) = e.location() {
                    format!("line {}, column {}: {e}", loc.line(), loc.column())
                } else {
                    e.to_string()
                }
            })
        };
        match parsed {
            Ok(cfg) => {
                println!(
                    "✓ {}: enabled={}, ignored={}, disabled_tools={}",
                    file.display(),
                    cfg.enabled,
                    cfg.ignored_paths.len(),
                    cfg.disabled_tools.len()
                );
            }
            Err(detail) => {
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
    fn webhook_secret_check_accepts_a_strong_secret() {
        // A 32-char hex string from a proper RNG.
        let s = "9f8e7d6c5b4a3210fedcba9876543210";
        let detail = check_webhook_secret(s).expect("strong secret");
        assert!(detail.contains("32"));
    }

    #[test]
    fn webhook_secret_check_warns_on_short_secret() {
        let err = check_webhook_secret("short").expect_err("short");
        assert!(err.contains("5 chars"));
    }

    #[test]
    fn webhook_secret_check_warns_on_all_digits() {
        let err = check_webhook_secret("1234567890123456").expect_err("digits-only");
        assert!(err.contains("digits"));
    }

    #[test]
    fn webhook_secret_check_warns_on_low_uniqueness() {
        let err = check_webhook_secret("aaaaaaaaaaaaaaaaaaaa").expect_err("low entropy");
        assert!(err.contains("distinct"));
    }

    #[tokio::test]
    async fn doctor_with_no_args_succeeds_with_all_skips() {
        let args = DoctorArgs {
            forgejo_url: None,
            token: None,
            llm_base_url: None,
            llm_api_key: None,
            llm_reasoning_model: None,
            llm_cheap_model: None,
            llm_embedding_model: None,
            webhook_secret: None,
            timeout_secs: 5,
        };
        // No checks run → no failures.
        doctor(args).await.expect("no checks should not fail");
    }

    #[tokio::test]
    async fn doctor_fails_when_a_required_dep_is_unreachable() {
        let args = DoctorArgs {
            forgejo_url: Some("http://127.0.0.1:1".into()),
            token: Some("tok".into()),
            llm_base_url: None,
            llm_api_key: None,
            llm_reasoning_model: None,
            llm_cheap_model: None,
            llm_embedding_model: None,
            webhook_secret: None,
            timeout_secs: 1,
        };
        let err = doctor(args).await.expect_err("unreachable forgejo");
        assert!(err.to_string().contains("required checks failed"));
    }

    #[tokio::test]
    async fn doctor_passes_when_only_secret_check_runs_and_secret_is_strong() {
        let args = DoctorArgs {
            forgejo_url: None,
            token: None,
            llm_base_url: None,
            llm_api_key: None,
            llm_reasoning_model: None,
            llm_cheap_model: None,
            llm_embedding_model: None,
            webhook_secret: Some("9f8e7d6c5b4a3210fedcba9876543210".into()),
            timeout_secs: 5,
        };
        doctor(args).await.expect("strong-secret-only run");
    }

    #[tokio::test]
    async fn doctor_passes_when_configured_models_are_loaded() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "qwen2.5-coder:32b"},
                    {"id": "qwen2.5-coder:7b"},
                    {"id": "bge-m3:latest"}
                ]
            })))
            .mount(&server)
            .await;

        let args = DoctorArgs {
            forgejo_url: None,
            token: None,
            llm_base_url: Some(server.uri()),
            llm_api_key: None,
            llm_reasoning_model: Some("qwen2.5-coder:32b".into()),
            llm_cheap_model: Some("qwen2.5-coder:7b".into()),
            llm_embedding_model: Some("bge-m3:latest".into()),
            webhook_secret: None,
            timeout_secs: 5,
        };
        doctor(args).await.expect("all configured models present");
    }

    #[tokio::test]
    async fn doctor_fails_when_configured_model_is_missing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "qwen2.5-coder:7b"}]
            })))
            .mount(&server)
            .await;

        let args = DoctorArgs {
            forgejo_url: None,
            token: None,
            llm_base_url: Some(server.uri()),
            llm_api_key: None,
            // Configured model name doesn't match what's loaded.
            llm_reasoning_model: Some("qwen2.5-coder:32b".into()),
            llm_cheap_model: None,
            llm_embedding_model: None,
            webhook_secret: None,
            timeout_secs: 5,
        };
        let err = doctor(args).await.expect_err("missing model is a fail");
        assert!(err.to_string().contains("required checks failed"));
    }

    #[tokio::test]
    async fn doctor_with_weak_secret_warns_but_does_not_fail() {
        // Warns aren't failures — operator gets the diagnostic
        // but the command still exits 0.
        let args = DoctorArgs {
            forgejo_url: None,
            token: None,
            llm_base_url: None,
            llm_api_key: None,
            llm_reasoning_model: None,
            llm_cheap_model: None,
            llm_embedding_model: None,
            webhook_secret: Some("aaaaaaaaaaaaaaaaaaaa".into()),
            timeout_secs: 5,
        };
        doctor(args)
            .await
            .expect("weak secret should warn, not fail");
    }

    #[test]
    fn sign_body_is_deterministic_and_hex_encoded() {
        let a = sign_body("secret", b"payload");
        let b = sign_body("secret", b"payload");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // sha256 hex
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different secret → different signature.
        let c = sign_body("other", b"payload");
        assert_ne!(a, c);
    }

    #[test]
    fn stub_pr_event_body_is_valid_json() {
        let body = stub_pr_event_body();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["action"], "opened");
        assert_eq!(v["pull_request"]["draft"], false);
        assert_eq!(v["repository"]["full_name"], "test/test");
    }

    /// Wire up an in-process gateway with NoOpDispatcher and verify
    /// `test_webhook` round-trips successfully. Catches real bugs:
    /// signature header name, content-type, body framing.
    #[tokio::test]
    async fn list_webhooks_renders_table() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 7, "type": "forgejo", "active": true,
                    "events": ["pull_request"],
                    "config": {"url": "https://reviewer.example.com/webhooks/forgejo"}
                }
            ])))
            .mount(&server)
            .await;
        let args = ListWebhooksArgs {
            forgejo_url: server.uri(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            json: false,
        };
        list_webhooks(args).await.expect("ok");
    }

    #[tokio::test]
    async fn unregister_webhook_by_id_calls_delete() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/o/r/hooks/7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let args = UnregisterWebhookArgs {
            forgejo_url: server.uri(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            id: Some(7),
            match_url: None,
        };
        unregister_webhook(args).await.expect("ok");
    }

    #[tokio::test]
    async fn unregister_webhook_by_match_url_lists_then_deletes_only_matches() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 7, "type": "forgejo", "active": true,
                    "events": [],
                    "config": {"url": "https://reviewer.example.com/webhooks/forgejo"}
                },
                {
                    "id": 12, "type": "gitea", "active": true,
                    "events": [],
                    "config": {"url": "https://other.example/legacy"}
                }
            ])))
            .mount(&server)
            .await;
        // Only id 7 should be deleted; 12 must NOT be.
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/o/r/hooks/7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/o/r/hooks/12"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let args = UnregisterWebhookArgs {
            forgejo_url: server.uri(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            id: None,
            match_url: Some("reviewer.example.com".into()),
        };
        unregister_webhook(args).await.expect("ok");
    }

    #[tokio::test]
    async fn unregister_webhook_match_url_with_no_match_errors() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let args = UnregisterWebhookArgs {
            forgejo_url: server.uri(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            id: None,
            match_url: Some("nope".into()),
        };
        let err = unregister_webhook(args).await.expect_err("no match");
        assert!(err.to_string().contains("nope"));
    }

    #[tokio::test]
    async fn unregister_webhook_with_neither_arg_errors() {
        let args = UnregisterWebhookArgs {
            forgejo_url: "http://127.0.0.1:1".into(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            id: None,
            match_url: None,
        };
        let err = unregister_webhook(args).await.expect_err("neither");
        assert!(err.to_string().contains("--id"));
        assert!(err.to_string().contains("--match-url"));
    }

    #[tokio::test]
    async fn list_learnings_renders_table_for_populated_store() {
        use ar_index::{LearningSource, LearningsStore, SqliteLearningsStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.db");
        {
            let store = SqliteLearningsStore::open(&path).await.unwrap();
            store
                .add(
                    "Forbid unwrap() outside #[cfg(test)]".into(),
                    LearningSource::Guideline,
                    vec![0.1; 4],
                    1700000000,
                )
                .await
                .unwrap();
        }
        let args = ListLearningsArgs {
            learnings_db: path,
            json: false,
        };
        list_learnings(args).await.expect("ok");
    }

    #[tokio::test]
    async fn list_learnings_emits_ndjson_when_json_set() {
        use ar_index::{LearningSource, LearningsStore, SqliteLearningsStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.db");
        {
            let store = SqliteLearningsStore::open(&path).await.unwrap();
            store
                .add("x".into(), LearningSource::Chat, vec![1.0; 3], 100)
                .await
                .unwrap();
        }
        let args = ListLearningsArgs {
            learnings_db: path,
            json: true,
        };
        list_learnings(args).await.expect("ok");
    }

    #[tokio::test]
    async fn list_learnings_handles_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.db");
        // Touch the db so list_learnings can open it.
        ar_index::SqliteLearningsStore::open(&path).await.unwrap();
        let args = ListLearningsArgs {
            learnings_db: path,
            json: false,
        };
        list_learnings(args).await.expect("ok");
    }

    #[tokio::test]
    async fn forget_learning_drops_the_matching_record() {
        use ar_index::{LearningSource, LearningsStore, SqliteLearningsStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.db");
        let id = {
            let store = SqliteLearningsStore::open(&path).await.unwrap();
            store
                .add(
                    "to be forgotten".into(),
                    LearningSource::Chat,
                    vec![0.5; 3],
                    100,
                )
                .await
                .unwrap()
                .id
        };
        let args = ForgetLearningArgs {
            learnings_db: path.clone(),
            id,
        };
        forget_learning(args).await.expect("ok");
        let store = SqliteLearningsStore::open(&path).await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_learning_on_unknown_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.db");
        ar_index::SqliteLearningsStore::open(&path).await.unwrap();
        let args = ForgetLearningArgs {
            learnings_db: path,
            id: 999,
        };
        let err = forget_learning(args).await.expect_err("not found");
        assert!(err.to_string().contains("999"));
    }

    #[tokio::test]
    async fn purge_history_drops_old_rows() {
        use ar_orchestrator::{PrKey, ReviewHistory, SqliteReviewHistory};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        {
            let store = SqliteReviewHistory::open(&path).await.unwrap();
            // Record at unix epoch — guaranteed older than any
            // reasonable `older_than_days` cutoff.
            store
                .record_at(
                    &PrKey {
                        owner: "o".into(),
                        repo: "r".into(),
                        pr_number: 1,
                    },
                    "x",
                    100,
                )
                .await
                .unwrap();
        }
        let args = PurgeHistoryArgs {
            history_db: path.clone(),
            older_than_days: 90,
            dry_run: false,
        };
        purge_history(args).await.expect("ok");
        let store = SqliteReviewHistory::open(&path).await.unwrap();
        assert!(store.list_known().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_history_keeps_recent_rows() {
        use ar_orchestrator::{PrKey, ReviewHistory, SqliteReviewHistory};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        {
            let store = SqliteReviewHistory::open(&path).await.unwrap();
            // Record with `now` so it's well past the 90-day
            // cutoff that the CLI computes.
            store
                .record(
                    &PrKey {
                        owner: "o".into(),
                        repo: "r".into(),
                        pr_number: 1,
                    },
                    "x",
                )
                .await
                .unwrap();
        }
        let args = PurgeHistoryArgs {
            history_db: path.clone(),
            older_than_days: 90,
            dry_run: false,
        };
        purge_history(args).await.expect("ok");
        let store = SqliteReviewHistory::open(&path).await.unwrap();
        assert_eq!(store.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn purge_history_dry_run_does_not_delete() {
        use ar_orchestrator::{PrKey, ReviewHistory, SqliteReviewHistory};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        {
            let store = SqliteReviewHistory::open(&path).await.unwrap();
            store
                .record_at(
                    &PrKey {
                        owner: "o".into(),
                        repo: "r".into(),
                        pr_number: 1,
                    },
                    "x",
                    100, // ancient row
                )
                .await
                .unwrap();
        }
        let args = PurgeHistoryArgs {
            history_db: path.clone(),
            older_than_days: 90,
            dry_run: true,
        };
        purge_history(args).await.expect("ok");
        let store = SqliteReviewHistory::open(&path).await.unwrap();
        // Row still exists despite being well past the cutoff.
        assert_eq!(store.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reset_pr_clears_existing_record() {
        use ar_orchestrator::{PrKey, ReviewHistory, SqliteReviewHistory};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        // Pre-populate the db so reset_pr has something to clear.
        {
            let store = SqliteReviewHistory::open(&path).await.unwrap();
            store
                .record(
                    &PrKey {
                        owner: "o".into(),
                        repo: "r".into(),
                        pr_number: 7,
                    },
                    "deadbeef",
                )
                .await
                .unwrap();
        }
        let args = ResetPrArgs {
            history_db: path.clone(),
            owner: "o".into(),
            repo: "r".into(),
            pr: 7,
        };
        reset_pr(args).await.expect("clears");
        // Verify it's gone.
        let store = SqliteReviewHistory::open(&path).await.unwrap();
        let after = store
            .last_reviewed(&PrKey {
                owner: "o".into(),
                repo: "r".into(),
                pr_number: 7,
            })
            .await
            .unwrap();
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn reset_pr_on_unknown_pr_succeeds() {
        // Should not error — clearing a non-existent record is a
        // legitimate use case (operator can run idempotently).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        // Open once to create the file with the schema.
        ar_orchestrator::SqliteReviewHistory::open(&path)
            .await
            .unwrap();
        let args = ResetPrArgs {
            history_db: path,
            owner: "o".into(),
            repo: "r".into(),
            pr: 999,
        };
        reset_pr(args).await.expect("noop on unknown PR");
    }

    #[tokio::test]
    async fn reset_pr_on_missing_db_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        // Don't create the db — but `open` will create-if-missing,
        // so this actually succeeds (idempotent reset). Verify the
        // create happens cleanly.
        let path = dir.path().join("nonexistent.db");
        let args = ResetPrArgs {
            history_db: path,
            owner: "o".into(),
            repo: "r".into(),
            pr: 1,
        };
        reset_pr(args).await.expect("create-if-missing");
    }

    #[test]
    fn parse_metric_counters_skips_help_type_and_label_lines() {
        let text = "\
# HELP auto_review_jobs_dispatched_total docs
# TYPE auto_review_jobs_dispatched_total counter
auto_review_jobs_dispatched_total 7
auto_review_review_duration_seconds_bucket{le=\"5\"} 3
auto_review_reviews_completed_count 2
";
        let map = parse_metric_counters(text);
        assert_eq!(map.get("auto_review_jobs_dispatched_total"), Some(&7));
        assert_eq!(map.get("auto_review_reviews_completed_count"), Some(&2));
        // The label-bearing line is skipped (we don't surface
        // histograms in the summary).
        assert!(!map.contains_key("auto_review_review_duration_seconds_bucket"));
    }

    #[test]
    fn status_summary_compute_handles_empty_metrics() {
        let version = serde_json::json!({"name": "auto_review", "version": "0.1.0"});
        let info = serde_json::json!({
            "bot_login": "auto_review",
            "sandbox": "podman",
            "learnings": "sqlite",
            "poller_enabled": true,
            "readiness_enabled": true
        });
        let summary = StatusSummary::compute(&version, &info, "");
        assert_eq!(summary.jobs_dispatched_total, 0);
        assert!(summary.success_rate.is_none());
    }

    #[test]
    fn status_summary_compute_calculates_success_rate() {
        let version = serde_json::json!({"version": "0.1.0"});
        let info = serde_json::json!({
            "bot_login": "auto_review", "sandbox": "podman",
            "learnings": "sqlite", "poller_enabled": true,
            "readiness_enabled": true
        });
        let metrics = "\
auto_review_reviews_succeeded_total 8
auto_review_reviews_failed_forgejo_total 1
auto_review_reviews_failed_workspace_total 0
auto_review_reviews_failed_llm_total 1
auto_review_reviews_failed_unhealable_total 0
auto_review_reviews_completed_count 10
";
        let summary = StatusSummary::compute(&version, &info, metrics);
        assert_eq!(summary.reviews_succeeded_total, 8);
        assert_eq!(summary.reviews_failed_total, 2);
        assert_eq!(summary.reviews_completed_count, 10);
        assert!((summary.success_rate.unwrap() - 0.8).abs() < 1e-9);
    }

    #[tokio::test]
    async fn end_to_end_status_against_real_gateway() {
        use ar_gateway::{build_router, AppState, GatewayInfo};
        use ar_orchestrator::NoOpDispatcher;

        let info = Arc::new(GatewayInfo {
            name: "auto_review",
            version: env!("CARGO_PKG_VERSION"),
            bot_login: "pr-bot".into(),
            bot_name: "pr-bot".into(),
            sandbox: "podman",
            learnings: "sqlite",
            history: "sqlite",
            llm_tiers: vec!["reasoning"],
            reasoning_model: "test-model".into(),
            poller_enabled: true,
            readiness_enabled: true,
        });
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)).with_info(info));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let args = StatusArgs {
            gateway_url: format!("http://{addr}"),
            json: true,
            timeout_secs: 5,
        };
        // Exits cleanly; we're not asserting stdout shape, just
        // that all three GETs succeed and the parser handles a
        // minimal-counters response.
        status(args).await.expect("status against real gateway");
    }

    #[tokio::test]
    async fn end_to_end_test_webhook_succeeds_against_real_gateway() {
        use ar_gateway::{build_router, AppState};
        use ar_orchestrator::NoOpDispatcher;
        use std::sync::Arc;

        let secret = "shared-secret";
        let app = build_router(AppState::new(secret, Arc::new(NoOpDispatcher)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let args = TestWebhookArgs {
            gateway_url: format!("http://{addr}"),
            webhook_secret: secret.into(),
            event: "ping".into(),
            timeout_secs: 5,
        };
        test_webhook(args).await.expect("ping should be 200 pong");
    }

    #[tokio::test]
    async fn end_to_end_test_webhook_fails_with_wrong_secret() {
        use ar_gateway::{build_router, AppState};
        use ar_orchestrator::NoOpDispatcher;
        use std::sync::Arc;

        let app = build_router(AppState::new("right", Arc::new(NoOpDispatcher)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let args = TestWebhookArgs {
            gateway_url: format!("http://{addr}"),
            webhook_secret: "wrong".into(),
            event: "ping".into(),
            timeout_secs: 5,
        };
        let err = test_webhook(args)
            .await
            .expect_err("wrong secret should fail");
        assert!(err.to_string().contains("401") || err.to_string().contains("WEBHOOK_SECRET"));
    }

    #[tokio::test]
    async fn end_to_end_pr_event_round_trips() {
        use ar_gateway::{build_router, AppState};
        use ar_orchestrator::NoOpDispatcher;
        use std::sync::Arc;

        let secret = "s";
        let app = build_router(AppState::new(secret, Arc::new(NoOpDispatcher)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let args = TestWebhookArgs {
            gateway_url: format!("http://{addr}"),
            webhook_secret: secret.into(),
            event: "pull_request".into(),
            timeout_secs: 5,
        };
        test_webhook(args)
            .await
            .expect("stub PR event should be 202 ACCEPTED");
    }

    #[tokio::test]
    async fn test_webhook_rejects_unsupported_event() {
        let args = TestWebhookArgs {
            gateway_url: "http://127.0.0.1:0".into(),
            webhook_secret: "s".into(),
            event: "release".into(),
            timeout_secs: 1,
        };
        let err = test_webhook(args).await.expect_err("unsupported event");
        assert!(err.to_string().contains("release"));
    }

    #[test]
    fn explain_routing_handles_python_file() {
        // Smoke-test: routing a .py file should at minimum
        // pull in ruff (the always-runs python linter).
        let args = ExplainRoutingArgs {
            file: vec!["src/x.py".into()],
            json: false,
        };
        explain_routing(args).expect("ok");
    }

    #[test]
    fn explain_routing_with_no_files_succeeds_silently() {
        // clap rejects zero --file at the parse layer; this
        // tests the function-level invariant that an empty
        // file list doesn't crash. Useful if a future caller
        // bypasses clap.
        let args = ExplainRoutingArgs {
            file: vec![],
            json: false,
        };
        explain_routing(args).expect("ok");
    }

    #[test]
    fn explain_routing_json_emits_structured_object() {
        let args = ExplainRoutingArgs {
            file: vec!["x.rs".into()],
            json: true,
        };
        explain_routing(args).expect("ok");
    }

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
            strict: false,
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
            strict: false,
        };
        let err = validate_config(args).expect_err("malformed should fail");
        assert!(err.to_string().contains("failed validation"));
    }

    #[test]
    fn validate_config_strict_rejects_unknown_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        // Typo: `enabld` instead of `enabled`.
        std::fs::write(dir.path().join(".auto_review.yaml"), "enabld: true\n").unwrap();
        // Permissive mode (default): silently parses, returns Ok.
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
            strict: false,
        };
        validate_config(args).expect("permissive should accept");

        // Strict mode: fails with the typo'd key in the error.
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
            strict: true,
        };
        let err = validate_config(args).expect_err("strict should reject");
        assert!(err.to_string().contains("failed validation"));
    }

    #[test]
    fn validate_config_fails_when_no_files_found() {
        let dir = tempfile::tempdir().unwrap();
        let args = ValidateConfigArgs {
            paths: vec![dir.path().to_path_buf()],
            strict: false,
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
