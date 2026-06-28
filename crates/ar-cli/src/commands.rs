//! Implementations of the CLI subcommands.

use crate::cli::{
    AgentcoreServeArgs, DoctorArgs, ForgetLearningArgs, InitArgs, ListLearningsArgs,
    ListWebhooksArgs, PurgeHistoryArgs, RegisterWebhookArgs, ResetPrArgs, ReviewOnceArgs,
    StatusArgs, TestWebhookArgs, UnregisterWebhookArgs, ValidateConfigArgs,
};
use anyhow::{Context, Result};
use ar_agentcore::{
    InvocationError, InvocationErrorKind, InvocationHandler, InvocationKind, InvocationOutcome,
    InvocationPayload, Provider,
};
use ar_chat::{parse_chat_command, ChatContext, ChatHandler};
use ar_forge::ReviewHost;
use ar_forgejo::{
    Client, CreateAccessTokenRequest, CreateWebhookRequest, InitClient, WebhookConfig,
};
use ar_github::{InstallationTokenRequest, Permission};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::{
    run_review_job, InMemoryReviewHistory, InlineDispatcher, JobDispatcher, ReviewHistory,
    ReviewJob,
};
use ar_prompts::{render_review_prompt, ReviewPromptInputs};
use ar_review::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;

const WEBHOOK_PATH: &str = "/webhooks/forgejo";

pub async fn agentcore_serve(args: AgentcoreServeArgs) -> Result<()> {
    let stores = build_agentcore_stores(&args).await?;
    ar_agentcore::serve(build_agentcore_serve_config(
        args,
        stores.idempotency,
        stores.history,
        stores.learnings,
    )?)
    .await
}

struct AgentcoreStores {
    idempotency: Option<Arc<dyn ar_agentcore::InvocationIdempotency>>,
    history: Option<Arc<dyn ar_orchestrator::ReviewHistory>>,
    learnings: Option<Arc<dyn ar_index::LearningsStore>>,
}

async fn build_agentcore_stores(args: &AgentcoreServeArgs) -> Result<AgentcoreStores> {
    if args.idempotency_dynamodb_table.is_none()
        && args.history_dynamodb_table.is_none()
        && args.learnings_dynamodb_table.is_none()
    {
        return Ok(AgentcoreStores {
            idempotency: None,
            history: None,
            learnings: None,
        });
    }

    let aws_config = aws_config::load_from_env().await;
    let dynamodb = aws_sdk_dynamodb::Client::new(&aws_config);
    let idempotency = args.idempotency_dynamodb_table.as_ref().map(|table_name| {
        Arc::new(ar_agentcore::DynamoDbInvocationIdempotency::new(
            dynamodb.clone(),
            table_name.clone(),
            args.idempotency_ttl_secs,
        )) as Arc<dyn ar_agentcore::InvocationIdempotency>
    });
    let history = args.history_dynamodb_table.as_ref().map(|table_name| {
        Arc::new(ar_orchestrator::DynamoDbReviewHistory::new(
            dynamodb.clone(),
            table_name.clone(),
        )) as Arc<dyn ar_orchestrator::ReviewHistory>
    });
    let learnings = args.learnings_dynamodb_table.as_ref().map(|table_name| {
        Arc::new(ar_index::DynamoDbLearningsStore::new(
            dynamodb.clone(),
            table_name.clone(),
        )) as Arc<dyn ar_index::LearningsStore>
    });

    Ok(AgentcoreStores {
        idempotency,
        history,
        learnings,
    })
}

fn build_agentcore_serve_config(
    args: AgentcoreServeArgs,
    idempotency: Option<Arc<dyn ar_agentcore::InvocationIdempotency>>,
    history: Option<Arc<dyn ar_orchestrator::ReviewHistory>>,
    learnings: Option<Arc<dyn ar_index::LearningsStore>>,
) -> Result<ar_agentcore::ServeConfig> {
    let forgejo_configured = args.forgejo_url.is_some() || args.token.is_some();
    let github_configured = args.github_app_id.is_some() || args.github_app_private_key.is_some();
    let handler = match (
        forgejo_configured,
        github_configured,
        args.llm_base_url.as_deref(),
    ) {
        (false, false, None) => None,
        (true, false, Some(llm_base_url)) => {
            let forgejo_url = args
                .forgejo_url
                .as_deref()
                .context("agentcore serve Forgejo mode requires --forgejo-url")?;
            let token = args
                .token
                .as_deref()
                .context("agentcore serve Forgejo mode requires --token")?;
            let forgejo =
                Arc::new(Client::new(forgejo_url, token).context("build Forgejo client")?);
            let llm =
                build_reasoning_llm(llm_base_url, args.llm_api_key.as_deref(), &args.llm_model)?;
            let host: Arc<dyn ReviewHost> = forgejo.clone();
            let mut dispatcher = InlineDispatcher::new_with_host(host, llm.clone());
            let chat_learnings = learnings
                .clone()
                .unwrap_or_else(|| Arc::new(ar_index::InMemoryLearningsStore::new()));
            if let Some(history) = history {
                dispatcher = dispatcher.with_history(history);
            }
            if let Some(learnings) = learnings {
                dispatcher = dispatcher.with_learnings(learnings);
            }
            let dispatcher = Arc::new(dispatcher);
            Some(Arc::new(ForgejoAgentcoreHandler {
                host: forgejo,
                dispatcher,
                llm,
                learnings: chat_learnings,
            }) as Arc<dyn ar_agentcore::InvocationHandler>)
        }
        (false, true, Some(llm_base_url)) => {
            let app_id = args
                .github_app_id
                .context("agentcore serve GitHub mode requires --github-app-id")?;
            let private_key = args
                .github_app_private_key
                .as_deref()
                .context("agentcore serve GitHub mode requires --github-app-private-key")?;
            let private_key = normalize_pem_from_env(private_key);
            let signer = ar_github::GitHubAppJwt::from_rsa_pem(app_id, private_key.as_bytes())
                .context("build GitHub App JWT signer")?;
            let llm =
                build_reasoning_llm(llm_base_url, args.llm_api_key.as_deref(), &args.llm_model)?;
            Some(Arc::new(GitHubAgentcoreSemanticReviewHandler {
                api_url: args.github_api_url,
                signer,
                llm,
                history: history.unwrap_or_else(|| Arc::new(InMemoryReviewHistory::new())),
                learnings,
            }) as Arc<dyn ar_agentcore::InvocationHandler>)
        }
        _ => {
            anyhow::bail!(
                "agentcore serve semantic-review mode requires either Forgejo (--forgejo-url, --token) or GitHub App (--github-app-id, --github-app-private-key) inputs plus --llm-base-url"
            );
        }
    };
    Ok(ar_agentcore::ServeConfig {
        bind: args.bind,
        handler,
        idempotency,
    })
}

fn build_reasoning_llm(
    llm_base_url: &str,
    llm_api_key: Option<&str>,
    llm_model: &str,
) -> Result<Arc<LlmRouter>> {
    let provider = Arc::new(
        OpenAiProvider::new(llm_base_url, llm_api_key, llm_model)
            .context("build reasoning LLM provider")?,
    );
    Ok(Arc::new(
        LlmRouter::new().with(ModelTier::Reasoning, provider),
    ))
}

fn normalize_pem_from_env(raw: &str) -> String {
    raw.replace("\\n", "\n")
}

struct ForgejoAgentcoreHandler {
    host: Arc<dyn ReviewHost>,
    dispatcher: Arc<dyn JobDispatcher>,
    llm: Arc<LlmRouter>,
    learnings: Arc<dyn ar_index::LearningsStore>,
}

#[async_trait]
impl InvocationHandler for ForgejoAgentcoreHandler {
    async fn handle(
        &self,
        payload: InvocationPayload,
    ) -> std::result::Result<InvocationOutcome, InvocationError> {
        if payload.provider != Provider::Forgejo {
            return Err(InvocationError {
                kind: InvocationErrorKind::InvalidPayload,
                message: "Forgejo runtime only accepts forgejo provider invocations".to_string(),
            });
        }

        match payload.kind {
            InvocationKind::SemanticReview => {
                handle_semantic_review(self.host.as_ref(), self.dispatcher.as_ref(), payload).await
            }
            InvocationKind::ChatCommand => {
                let comment_body =
                    payload
                        .comment_body
                        .as_deref()
                        .ok_or_else(|| InvocationError {
                            kind: InvocationErrorKind::InvalidPayload,
                            message: "chat_command invocations require comment_body".to_string(),
                        })?;
                let command = parse_chat_command(comment_body, "auto-review");
                let handler = ChatHandler {
                    host: self.host.as_ref(),
                    llm: self.llm.as_ref(),
                    learnings: self.learnings.as_ref(),
                    dispatcher: Some(self.dispatcher.clone()),
                };
                handler
                    .handle(
                        ChatContext {
                            owner: &payload.owner,
                            repo: &payload.repo,
                            issue_number: payload.pr_number,
                            commenter_login: "agentcore",
                            bot_login: "auto-review",
                        },
                        command,
                    )
                    .await
                    .map_err(|error| InvocationError {
                        kind: InvocationErrorKind::ExecutionFailed,
                        message: format!("handle chat command: {error}"),
                    })?;

                Ok(InvocationOutcome {
                    status: "handled".to_string(),
                    message: "chat command handled".to_string(),
                })
            }
        }
    }
}

async fn handle_semantic_review(
    host: &dyn ReviewHost,
    dispatcher: &dyn JobDispatcher,
    payload: InvocationPayload,
) -> std::result::Result<InvocationOutcome, InvocationError> {
    let pr = host
        .get_pull_request(&payload.owner, &payload.repo, payload.pr_number)
        .await
        .map_err(|error| InvocationError {
            kind: InvocationErrorKind::ExecutionFailed,
            message: format!("fetch pull request: {error}"),
        })?;
    if pr.head.sha != payload.head_sha {
        return Err(InvocationError {
            kind: InvocationErrorKind::StaleHead,
            message: format!(
                "payload head_sha {} does not match current PR head {}",
                payload.head_sha, pr.head.sha
            ),
        });
    }

    dispatcher
        .dispatch(ReviewJob {
            owner: payload.owner,
            repo: payload.repo,
            pr_number: payload.pr_number,
            head_sha: payload.head_sha,
            pr_title: pr.title,
            pr_body: pr.body,
            force: payload.force.unwrap_or(false),
        })
        .await;

    Ok(InvocationOutcome {
        status: "completed".to_string(),
        message: "semantic review completed".to_string(),
    })
}

struct GitHubAgentcoreSemanticReviewHandler {
    api_url: String,
    signer: ar_github::GitHubAppJwt,
    llm: Arc<LlmRouter>,
    history: Arc<dyn ReviewHistory>,
    learnings: Option<Arc<dyn ar_index::LearningsStore>>,
}

#[async_trait]
impl InvocationHandler for GitHubAgentcoreSemanticReviewHandler {
    async fn handle(
        &self,
        payload: InvocationPayload,
    ) -> std::result::Result<InvocationOutcome, InvocationError> {
        if payload.provider != Provider::Github {
            return Err(InvocationError {
                kind: InvocationErrorKind::InvalidPayload,
                message: "GitHub runtime only accepts github provider invocations".to_string(),
            });
        }
        match payload.kind {
            InvocationKind::SemanticReview => self.handle_semantic_review(payload).await,
            InvocationKind::ChatCommand => self.handle_chat_command(payload).await,
        }
    }
}

impl GitHubAgentcoreSemanticReviewHandler {
    async fn host_for_payload(
        &self,
        payload: &InvocationPayload,
    ) -> std::result::Result<Arc<dyn ReviewHost>, InvocationError> {
        let installation_id = payload.installation_id.ok_or_else(|| InvocationError {
            kind: InvocationErrorKind::InvalidPayload,
            message: "github invocations require installation_id".to_string(),
        })?;
        let app_jwt = self.signer.jwt_now().map_err(|error| InvocationError {
            kind: InvocationErrorKind::ExecutionFailed,
            message: format!("sign GitHub App JWT: {error}"),
        })?;
        let github =
            ar_github::Client::new(&self.api_url, &app_jwt).map_err(|error| InvocationError {
                kind: InvocationErrorKind::ExecutionFailed,
                message: format!("build GitHub client: {error}"),
            })?;
        let request = InstallationTokenRequest::for_repository(&payload.repo)
            .with_permission("contents", Permission::Read)
            .with_permission("issues", Permission::Write)
            .with_permission("pull_requests", Permission::Write)
            .with_permission("statuses", Permission::Write);
        let token = github
            .installation_token(installation_id, request)
            .await
            .map_err(|error| InvocationError {
                kind: InvocationErrorKind::ExecutionFailed,
                message: format!("create GitHub installation token: {error}"),
            })?;
        Ok(Arc::new(ar_github::InstallationReviewHost::new(
            github,
            token.token,
        )))
    }

    async fn handle_semantic_review(
        &self,
        payload: InvocationPayload,
    ) -> std::result::Result<InvocationOutcome, InvocationError> {
        let host = self.host_for_payload(&payload).await?;
        let pr = host
            .get_pull_request(&payload.owner, &payload.repo, payload.pr_number)
            .await
            .map_err(|error| InvocationError {
                kind: InvocationErrorKind::ExecutionFailed,
                message: format!("fetch pull request: {error}"),
            })?;
        if pr.head.sha != payload.head_sha {
            return Err(InvocationError {
                kind: InvocationErrorKind::StaleHead,
                message: format!(
                    "payload head_sha {} does not match current PR head {}",
                    payload.head_sha, pr.head.sha
                ),
            });
        }

        let mut dispatcher = InlineDispatcher::new_with_host(host, self.llm.clone())
            .with_history(self.history.clone());
        if let Some(learnings) = &self.learnings {
            dispatcher = dispatcher.with_learnings(learnings.clone());
        }
        dispatcher
            .dispatch(ReviewJob {
                owner: payload.owner,
                repo: payload.repo,
                pr_number: payload.pr_number,
                head_sha: payload.head_sha,
                pr_title: pr.title,
                pr_body: pr.body,
                force: payload.force.unwrap_or(false),
            })
            .await;

        Ok(InvocationOutcome {
            status: "completed".to_string(),
            message: "semantic review completed".to_string(),
        })
    }

    async fn handle_chat_command(
        &self,
        payload: InvocationPayload,
    ) -> std::result::Result<InvocationOutcome, InvocationError> {
        let comment_body = payload
            .comment_body
            .as_deref()
            .ok_or_else(|| InvocationError {
                kind: InvocationErrorKind::InvalidPayload,
                message: "chat_command invocations require comment_body".to_string(),
            })?;
        let host = self.host_for_payload(&payload).await?;
        let command = parse_chat_command(comment_body, "auto-review");
        let mut dispatcher = InlineDispatcher::new_with_host(host.clone(), self.llm.clone())
            .with_history(self.history.clone());
        if let Some(learnings) = &self.learnings {
            dispatcher = dispatcher.with_learnings(learnings.clone());
        }
        let fallback_learnings;
        let learnings: &dyn ar_index::LearningsStore = match self.learnings.as_deref() {
            Some(learnings) => learnings,
            None => {
                fallback_learnings = ar_index::InMemoryLearningsStore::new();
                &fallback_learnings
            }
        };
        let handler = ChatHandler {
            host: host.as_ref(),
            llm: self.llm.as_ref(),
            learnings,
            dispatcher: Some(Arc::new(dispatcher)),
        };
        handler
            .handle(
                ChatContext {
                    owner: &payload.owner,
                    repo: &payload.repo,
                    issue_number: payload.pr_number,
                    commenter_login: "agentcore",
                    bot_login: "auto-review",
                },
                command,
            )
            .await
            .map_err(|error| InvocationError {
                kind: InvocationErrorKind::ExecutionFailed,
                message: format!("handle chat command: {error}"),
            })?;

        Ok(InvocationOutcome {
            status: "handled".to_string(),
            message: "chat command handled".to_string(),
        })
    }
}

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
    println!("    export AR_FORGEJO_TOKEN={}", token.sha1);
    println!();
    println!("Recommended next step:");
    println!("    auto-review webhook register --owner OWNER --repo REPO \\");
    println!("        --forgejo-url {} \\", args.forgejo_url);
    println!("        --token \"$AR_FORGEJO_TOKEN\" \\");
    println!("        --gateway-url https://reviewer.example.com \\");
    println!("        --webhook-secret \"$(openssl rand -hex 32)\"");
    println!();
    println!("(webhook register rejects secrets shorter than 16 bytes; the");
    println!(" gateway warns when WEBHOOK_SECRET is similarly short.)");
    Ok(())
}

/// Run a one-shot reasoning-path review against a specific PR. This builds a
/// Forgejo client plus reasoning-tier LLM provider and invokes the orchestrator
/// synchronously (no spawn) so the user can observe the outcome in their
/// terminal; it does not wire every gateway runtime store or optional tier.
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
    run_review_job(
        forgejo.as_ref(),
        &llm,
        &args.forgejo_url,
        &args.token,
        &history,
        // review-once is a one-shot debug command — no learnings
        // store wired in. Future: take a path to a SQLite file.
        None,
        // Same: no shared vector store. Each invocation builds a
        // fresh in-memory store via build_review_context's back-compat
        // path, then drops it.
        None,
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
        previous_review_sha: None,
        guidelines: "",
        repo_context: "",
        prior_discussion: "",
    });
    println!("{prompt}");
    Ok(())
}

/// List every webhook installed on the repo. Operators use this
/// to audit which webhooks the bot's PAT can see and to find the
/// id `webhook unregister` needs.
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
    // Reject obviously-weak secrets at registration time.
    // Forgejo accepts any string (including empty) as a webhook
    // secret, but an empty or short secret means anyone who knows
    // the gateway URL can forge a valid signature: HMAC-SHA256
    // with a known/short key is brute-forceable. The gateway logs
    // a similar warning at startup; surfacing it here too means
    // the operator hits the message before pushing the bad secret
    // to Forgejo.
    if args.webhook_secret.trim().is_empty() {
        anyhow::bail!(
            "--webhook-secret is empty; refuse to register a webhook with no \
             HMAC protection. Generate one with `openssl rand -hex 32`."
        );
    }
    if args.webhook_secret.len() < 16 {
        anyhow::bail!(
            "--webhook-secret is only {} bytes; refuse to register because the \
             HMAC is weakly resistant to brute force. Recommend 32+ random \
             bytes (e.g. `openssl rand -hex 32`).",
            args.webhook_secret.len()
        );
    }
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
    pub learnings: String,
    pub history: String,
    pub runtime_isolation: RuntimeIsolationSummary,
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

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeIsolationSummary {
    pub kind: String,
    pub label: String,
    pub detail: String,
}

impl RuntimeIsolationSummary {
    fn oci_default() -> Self {
        Self {
            kind: "oci_default".to_string(),
            label: "packaged OCI container isolation".to_string(),
            detail: "Gateway uses embedded OCI container-equivalent isolation by default."
                .to_string(),
        }
    }

    fn explicit_bare() -> Self {
        Self {
            kind: "explicit_bare".to_string(),
            label: "bare gateway mode".to_string(),
            detail: explicit_bare_gateway_mode_warning().to_string(),
        }
    }

    fn unsupported_platform() -> Self {
        Self {
            kind: "unsupported_platform".to_string(),
            label: "unsupported platform".to_string(),
            detail:
                "Embedded OCI isolation is unavailable on this platform; run in bare mode or provide external isolation."
                    .to_string(),
        }
    }

    fn external_container() -> Self {
        Self {
            kind: "external_container".to_string(),
            label: "external container isolation".to_string(),
            detail: "Gateway is already inside an externally provided container boundary."
                .to_string(),
        }
    }

    fn from_info(info: &serde_json::Value) -> Self {
        let Some(posture) = info.get("runtime_isolation") else {
            return Self::oci_default();
        };
        Self {
            kind: posture["kind"].as_str().unwrap_or("unknown").to_string(),
            label: posture["label"].as_str().unwrap_or("unknown").to_string(),
            detail: posture["detail"].as_str().unwrap_or("unknown").to_string(),
        }
    }
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
            learnings: info["learnings"].as_str().unwrap_or("unknown").to_string(),
            history: info["history"].as_str().unwrap_or("unknown").to_string(),
            runtime_isolation: RuntimeIsolationSummary::from_info(info),
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
        print!("{}", self.render_for_operator(base));
    }

    fn render_for_operator(&self, base: &str) -> String {
        let poller = if self.poller_enabled {
            "running"
        } else {
            "disabled"
        };
        let readiness = if self.readiness_enabled {
            "enabled"
        } else {
            "fallback to /healthz"
        };
        let success_rate = match self.success_rate {
            Some(r) => format!("{:.1}%", r * 100.0),
            None => "— (no completions yet)".to_string(),
        };
        format!(
            "auto_review status — {base}\n  version          {version}\n  bot login        {bot_login}\n  learnings        {learnings}\n  history          {history}\n  poller           {poller}\n  readiness probe  {readiness}\n\nRuntime isolation:\n  posture          {posture}\n  detail           {posture_detail}\n\nReview pipeline:\n  jobs dispatched  {jobs}\n  succeeded        {succeeded}\n  failed           {failed}\n  skipped          {skipped}\n  success rate     {success_rate}\n\nWebhook intake (rejection counters):\n  signature fails  {signature_failures}\n  payload fails    {payload_failures}\n  rate-limited     {rate_limited}\n\nPoller:\n  cycles total     {poll_cycles}\n",
            version = self.version,
            bot_login = self.bot_login,
            learnings = self.learnings,
            history = self.history,
            posture = self.runtime_isolation.label,
            posture_detail = self.runtime_isolation.detail,
            jobs = self.jobs_dispatched_total,
            succeeded = self.reviews_succeeded_total,
            failed = self.reviews_failed_total,
            skipped = self.reviews_skipped_total,
            signature_failures = self.webhook_signature_failures_total,
            payload_failures = self.webhook_payload_failures_total,
            rate_limited = self.webhook_rate_limited_total,
            poll_cycles = self.poll_cycles_total,
        )
    }
}

fn classify_local_runtime_isolation_posture(
    bare: Option<&str>,
    external_isolation: Option<&str>,
) -> Result<RuntimeIsolationSummary> {
    if std::env::consts::OS != "linux" {
        return Ok(RuntimeIsolationSummary::unsupported_platform());
    }
    if external_isolation == Some("container") {
        return Ok(RuntimeIsolationSummary::external_container());
    }
    match bare.map(str::trim).map(str::to_ascii_lowercase) {
        None => Ok(RuntimeIsolationSummary::oci_default()),
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => {
            Ok(RuntimeIsolationSummary::explicit_bare())
        }
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => {
            Ok(RuntimeIsolationSummary::oci_default())
        }
        Some(_) => anyhow::bail!(
            "AR_GATEWAY_BARE has an unrecognized value; use true/false, yes/no, on/off, or 1/0"
        ),
    }
}

fn explicit_bare_gateway_mode_warning() -> &'static str {
    "Warning: bare gateway mode selected; only application-level controls are active, not container-equivalent isolation."
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

    let bare = std::env::var("AR_GATEWAY_BARE").ok();
    let external_isolation = std::env::var("AR_GATEWAY_EXTERNAL_ISOLATION").ok();
    report
        .add_runtime_isolation_posture_from_env(bare.as_deref(), external_isolation.as_deref())?;

    // Git: required for the workspace clone phase. Without it, every
    // review fails at prepare_workspace with a confusing
    // "No such file or directory" io error from
    // Command::new("git"). Catch the missing-git case here so
    // operators see a clear "install git" message instead of
    // chasing an opaque os error.
    match probe_git(timeout).await {
        Ok(version) => report.pass("git", format!("{version} (in PATH)")),
        Err(e) => report.fail(
            "git",
            format!("{e} — install git or add it to PATH (every review needs `git clone`)"),
        ),
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

async fn probe_git(timeout: std::time::Duration) -> Result<String> {
    let fut = tokio::process::Command::new("git")
        .arg("--version")
        .output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .context("git --version timeout")?
        .context("spawn git --version (is git installed and on PATH?)")?;
    if !output.status.success() {
        anyhow::bail!(
            "git --version exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
    #[cfg(test)]
    fn add_runtime_isolation_posture(&mut self, bare: Option<&str>) -> Result<()> {
        self.add_runtime_isolation_posture_from_env(bare, None)
    }
    fn add_runtime_isolation_posture_from_env(
        &mut self,
        bare: Option<&str>,
        external_isolation: Option<&str>,
    ) -> Result<()> {
        let posture = classify_local_runtime_isolation_posture(bare, external_isolation)?;
        if posture.kind == "explicit_bare" {
            self.warn("runtime-isolation", posture.detail);
        } else if posture.kind == "oci_default" {
            self.warn(
                "runtime-isolation",
                format!(
                    "{}; gateway will attempt embedded OCI isolation, but prerequisites are not verified by doctor",
                    posture.label
                ),
            );
        } else {
            self.pass(
                "runtime-isolation",
                format!("{}; {}", posture.label, posture.detail),
            );
        }
        Ok(())
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
            "title": "auto-review webhook test (stub event)",
            "body": "synthetic event from `auto-review webhook test`",
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
                    "✓ {}: enabled={}, ignored={}",
                    file.display(),
                    cfg.enabled,
                    cfg.ignored_paths.len()
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
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn agentcore_serve_config_uses_forgejo_runtime_inputs_for_handler() {
        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: Some("https://git.example".to_string()),
                token: Some("forgejo-token".to_string()),
                github_api_url: "https://api.github.com".to_string(),
                github_app_id: None,
                github_app_private_key: None,
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: None,
                learnings_dynamodb_table: None,
            },
            None,
            None,
            None,
        )
        .expect("serve config");

        assert_eq!(config.bind, "127.0.0.1:0");
        assert!(
            config.handler.is_some(),
            "Forgejo runtime inputs should produce a handler-backed AgentCore server"
        );
    }

    #[test]
    fn agentcore_serve_config_uses_github_app_runtime_inputs_for_handler() {
        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: None,
                token: None,
                github_api_url: "https://api.github.example".to_string(),
                github_app_id: Some(12345),
                github_app_private_key: Some(test_github_private_key().to_string()),
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: None,
                learnings_dynamodb_table: None,
            },
            None,
            None,
            None,
        )
        .expect("serve config");

        assert_eq!(config.bind, "127.0.0.1:0");
        assert!(
            config.handler.is_some(),
            "GitHub App runtime inputs should produce a handler-backed AgentCore server"
        );
    }

    #[tokio::test]
    async fn forgejo_agentcore_chat_command_posts_help_comment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_string_contains("auto_review chat commands"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: Some(server.uri()),
                token: Some("forgejo-token".to_string()),
                github_api_url: "https://api.github.com".to_string(),
                github_app_id: None,
                github_app_private_key: None,
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: None,
                learnings_dynamodb_table: None,
            },
            None,
            None,
            None,
        )
        .expect("serve config");
        let handler = config.handler.expect("handler");

        let outcome = handler
            .handle(InvocationPayload {
                provider: Provider::Forgejo,
                kind: InvocationKind::ChatCommand,
                owner: "alice".to_string(),
                repo: "widgets".to_string(),
                pr_number: 42,
                head_sha: "head-sha".to_string(),
                installation_id: None,
                force: None,
                comment_id: Some(99),
                comment_body: Some("@auto-review help".to_string()),
            })
            .await
            .expect("chat command handled");

        assert_eq!(outcome.status, "handled");
        assert_eq!(outcome.message, "chat command handled");
    }

    #[tokio::test]
    async fn github_agentcore_chat_command_posts_help_comment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/installations/42/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": "installation-token",
                "expires_at": "2099-01-01T00:00:00Z"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/alice/widgets/issues/42/comments"))
            .and(body_string_contains("auto_review chat commands"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: None,
                token: None,
                github_api_url: server.uri(),
                github_app_id: Some(12345),
                github_app_private_key: Some(test_github_private_key().to_string()),
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: None,
                learnings_dynamodb_table: None,
            },
            None,
            None,
            None,
        )
        .expect("serve config");
        let handler = config.handler.expect("handler");

        let outcome = handler
            .handle(InvocationPayload {
                provider: Provider::Github,
                kind: InvocationKind::ChatCommand,
                owner: "alice".to_string(),
                repo: "widgets".to_string(),
                pr_number: 42,
                head_sha: "head-sha".to_string(),
                installation_id: Some(42),
                force: None,
                comment_id: Some(99),
                comment_body: Some("@auto-review help".to_string()),
            })
            .await
            .expect("chat command handled");

        assert_eq!(outcome.status, "handled");
        assert_eq!(outcome.message, "chat command handled");
    }

    fn test_github_private_key() -> &'static str {
        let source = include_str!("../../ar-github/tests/app_jwt.rs");
        source
            .split("const TEST_PRIVATE_KEY: &str = r#\"")
            .nth(1)
            .and_then(|rest| rest.split("\"#;").next())
            .expect("test key in ar-github app_jwt test")
    }

    #[tokio::test]
    async fn github_agentcore_handler_requires_installation_id_before_network() {
        let signer =
            ar_github::GitHubAppJwt::from_rsa_pem(12345, test_github_private_key().as_bytes())
                .expect("signer");
        let handler = GitHubAgentcoreSemanticReviewHandler {
            api_url: "https://api.github.invalid".to_string(),
            signer,
            llm: Arc::new(LlmRouter::new()),
            history: Arc::new(InMemoryReviewHistory::new()),
            learnings: None,
        };

        let error = handler
            .handle(InvocationPayload {
                provider: Provider::Github,
                kind: InvocationKind::SemanticReview,
                owner: "alice".to_string(),
                repo: "widgets".to_string(),
                pr_number: 42,
                head_sha: "head-sha".to_string(),
                installation_id: None,
                force: None,
                comment_id: None,
                comment_body: None,
            })
            .await
            .expect_err("missing installation_id should be rejected");

        assert_eq!(error.kind, InvocationErrorKind::InvalidPayload);
        assert!(
            error.message.contains("installation_id"),
            "operator-facing error should name the missing field, got: {}",
            error.message
        );
    }

    #[test]
    fn agentcore_serve_config_carries_selected_idempotency_store() {
        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: Some("https://git.example".to_string()),
                token: Some("forgejo-token".to_string()),
                github_api_url: "https://api.github.com".to_string(),
                github_app_id: None,
                github_app_private_key: None,
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: Some("agentcore-idempotency".to_string()),
                idempotency_ttl_secs: 900,
                history_dynamodb_table: None,
                learnings_dynamodb_table: None,
            },
            Some(Arc::new(ar_agentcore::InMemoryInvocationIdempotency::new())),
            None,
            None,
        )
        .expect("serve config");

        assert!(
            config.idempotency.is_some(),
            "selected idempotency store should be passed to AgentCore runtime"
        );
    }

    #[test]
    fn agentcore_serve_config_accepts_selected_history_store() {
        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: Some("https://git.example".to_string()),
                token: Some("forgejo-token".to_string()),
                github_api_url: "https://api.github.com".to_string(),
                github_app_id: None,
                github_app_private_key: None,
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: Some("agentcore-history".to_string()),
                learnings_dynamodb_table: None,
            },
            None,
            Some(Arc::new(ar_orchestrator::InMemoryReviewHistory::new())),
            None,
        )
        .expect("serve config");

        assert!(
            config.handler.is_some(),
            "selected history store should still produce a handler-backed AgentCore server"
        );
    }

    #[test]
    fn agentcore_serve_config_accepts_selected_learnings_store() {
        let config = build_agentcore_serve_config(
            AgentcoreServeArgs {
                bind: "127.0.0.1:0".to_string(),
                forgejo_url: Some("https://git.example".to_string()),
                token: Some("forgejo-token".to_string()),
                github_api_url: "https://api.github.com".to_string(),
                github_app_id: None,
                github_app_private_key: None,
                llm_base_url: Some("https://llm.example/v1".to_string()),
                llm_api_key: None,
                llm_model: "review-model".to_string(),
                idempotency_dynamodb_table: None,
                idempotency_ttl_secs: 86_400,
                history_dynamodb_table: None,
                learnings_dynamodb_table: Some("agentcore-learnings".to_string()),
            },
            None,
            None,
            Some(Arc::new(ar_index::InMemoryLearningsStore::new())),
        )
        .expect("serve config");

        assert!(
            config.handler.is_some(),
            "selected learnings store should still produce a handler-backed AgentCore server"
        );
    }

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
    fn doctor_report_warns_when_local_posture_is_explicit_bare() {
        let mut report = DoctorReport::new();

        report.add_runtime_isolation_posture(Some("true")).unwrap();

        let posture = report
            .results
            .iter()
            .find(|result| result.name == "runtime-isolation")
            .expect("doctor should include local runtime isolation posture");
        assert_eq!(posture.status, CheckStatus::Warn);
        assert!(
            posture.detail.contains("bare gateway mode"),
            "bare-mode posture should name the selected mode: {posture:?}"
        );
        assert!(
            posture
                .detail
                .contains("only application-level controls are active"),
            "bare-mode posture should warn about the limited local controls: {posture:?}"
        );
        assert!(
            !posture
                .detail
                .contains("container-equivalent isolation is active"),
            "doctor must not claim explicit bare mode has container-equivalent isolation: \
             {posture:?}"
        );
    }

    #[test]
    fn doctor_report_warns_when_default_runtime_isolation_is_not_verified() {
        let mut report = DoctorReport::new();

        report.add_runtime_isolation_posture(None).unwrap();

        let posture = report
            .results
            .iter()
            .find(|result| result.name == "runtime-isolation")
            .expect("doctor should include local runtime isolation posture");
        assert_eq!(posture.status, CheckStatus::Warn);
        assert!(
            posture.detail.contains("will attempt"),
            "default posture should describe the unverified runtime attempt: {posture:?}"
        );
        assert!(
            posture.detail.contains("not verified"),
            "default posture should say embedded OCI prerequisites are not verified: {posture:?}"
        );
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
    async fn register_webhook_rejects_empty_secret_before_calling_forgejo() {
        // Defence: a webhook with an empty HMAC secret means anyone
        // who learns the gateway URL can spoof reviews. Reject at
        // the CLI rather than registering the bad config.
        use crate::cli::RegisterWebhookArgs;
        let args = RegisterWebhookArgs {
            forgejo_url: "http://invalid.example".into(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            gateway_url: "http://gw.example".into(),
            webhook_secret: "".into(),
        };
        let err = register_webhook(args).await.expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("--webhook-secret is empty"),
            "expected empty-secret message, got: {msg}"
        );
    }

    #[tokio::test]
    async fn register_webhook_rejects_short_secret_before_calling_forgejo() {
        use crate::cli::RegisterWebhookArgs;
        let args = RegisterWebhookArgs {
            forgejo_url: "http://invalid.example".into(),
            token: "tok".into(),
            owner: "o".into(),
            repo: "r".into(),
            gateway_url: "http://gw.example".into(),
            webhook_secret: "shorty".into(),
        };
        let err = register_webhook(args).await.expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("only 6 bytes"),
            "expected length-pointing message, got: {msg}"
        );
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
            "learnings": "sqlite",
            "poller_enabled": true,
            "readiness_enabled": true
        });
        let summary = StatusSummary::compute(&version, &info, "");
        assert_eq!(summary.jobs_dispatched_total, 0);
        assert!(summary.success_rate.is_none());
        let json = serde_json::to_value(&summary).unwrap();
        assert!(
            json.get("sandbox").is_none(),
            "status JSON should not preserve the removed gateway sandbox surface: {json}"
        );
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

    #[test]
    fn status_summary_compute_renders_info_runtime_isolation_posture() {
        let version = serde_json::json!({"version": "0.1.0"});
        let info = serde_json::json!({
            "bot_login": "auto_review",
            "learnings": "sqlite",
            "history": "sqlite",
            "poller_enabled": true,
            "readiness_enabled": true,
            "runtime_isolation": {
                "kind": "explicit_bare",
                "label": "bare gateway mode",
                "detail": "Warning: bare gateway mode selected; only application-level controls are active, not container-equivalent isolation."
            }
        });

        let summary = StatusSummary::compute(&version, &info, "");

        assert_eq!(summary.runtime_isolation.kind, "explicit_bare");
        assert_eq!(summary.runtime_isolation.label, "bare gateway mode");
        assert!(summary
            .runtime_isolation
            .detail
            .contains("only application-level controls are active"));
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["runtime_isolation"]["kind"], "explicit_bare");
        assert_eq!(json["runtime_isolation"]["label"], "bare gateway mode");
    }

    #[test]
    fn status_summary_operator_rendering_includes_runtime_isolation_posture() {
        let version = serde_json::json!({"version": "0.1.0"});
        let info = serde_json::json!({
            "bot_login": "auto_review",
            "learnings": "sqlite",
            "history": "sqlite",
            "poller_enabled": true,
            "readiness_enabled": true,
            "runtime_isolation": {
                "kind": "explicit_bare",
                "label": "bare gateway mode",
                "detail": "Warning: bare gateway mode selected; only application-level controls are active, not container-equivalent isolation."
            }
        });

        let summary = StatusSummary::compute(&version, &info, "");
        let rendered = summary.render_for_operator("http://gateway.example");

        assert!(
            rendered.contains("Runtime isolation"),
            "operator status output should include a runtime isolation section: {rendered}"
        );
        assert!(
            rendered.contains("bare gateway mode"),
            "operator status output should render the /info runtime isolation label: {rendered}"
        );
        assert!(
            rendered.contains("only application-level controls are active"),
            "operator status output should render the posture warning detail: {rendered}"
        );
    }

    #[tokio::test]
    async fn end_to_end_status_against_real_gateway() {
        use ar_gateway::{build_router, AppState, GatewayInfo, RuntimeIsolationPostureInfo};
        use ar_orchestrator::NoOpDispatcher;

        let info = Arc::new(GatewayInfo {
            name: "auto_review",
            version: env!("CARGO_PKG_VERSION"),
            bot_login: "pr-bot".into(),
            bot_name: "pr-bot".into(),
            learnings: "sqlite".into(),
            history: "sqlite".into(),
            vector: "sqlite".into(),
            dedup: "sqlite".into(),
            llm_tiers: vec!["reasoning"],
            reasoning_model: "test-model".into(),
            poller_enabled: true,
            readiness_enabled: true,
            runtime_isolation: RuntimeIsolationPostureInfo::oci_default(),
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
