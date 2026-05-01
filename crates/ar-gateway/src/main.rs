use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::dedup::RecentDeliveries;
use ar_gateway::metrics::{Metrics, MetricsObserver};
use ar_gateway::poller::{ChatPoller, DEFAULT_POLL_INTERVAL};
use ar_gateway::ratelimit::TokenBucket;
use ar_gateway::{build_router, AppState, ChatDeps, GatewayInfo, ReadinessProbe};
use ar_index::{InMemoryLearningsStore, SqliteLearningsStore};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::review_history::{InMemoryReviewHistory, ReviewHistory};
use ar_orchestrator::sqlite_history::SqliteReviewHistory;
use ar_orchestrator::SpawningDispatcher;
use ar_sandbox::{DirectSandbox, PodmanSandbox, PodmanSandboxConfig, Sandbox};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ar_gateway=debug")),
        )
        .init();

    let bind = env::var("AR_GATEWAY_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let secret = env::var("WEBHOOK_SECRET").context("WEBHOOK_SECRET is required")?;
    // Forgejo's webhook docs recommend a strong random secret; HMAC-
    // SHA256 with a short shared key is brute-forceable. Warn at
    // startup rather than at first verify so operators see this in
    // their first log scrape, not a production failure window.
    if secret.len() < 16 {
        tracing::warn!(
            length = secret.len(),
            "WEBHOOK_SECRET is shorter than 16 bytes; HMAC verification \
             will work but the secret is weakly resistant to brute-force \
             attack. Recommend 32+ random bytes (e.g. `openssl rand -hex 32`)"
        );
    }
    let forgejo_base = env::var("FORGEJO_BASE_URL").context("FORGEJO_BASE_URL is required")?;
    let forgejo_token = env::var("FORGEJO_TOKEN").context("FORGEJO_TOKEN is required")?;
    let llm_base = env::var("LLM_BASE_URL").context("LLM_BASE_URL is required")?;
    let llm_api_key = env::var("LLM_API_KEY").ok();
    let reasoning_model =
        env::var("LLM_REASONING_MODEL").unwrap_or_else(|_| "qwen2.5-coder:32b".into());
    // An empty string is a more confusing failure mode than a
    // missing variable, because clap-style "missing required" never
    // fires (env::var returns Ok("")) and every subsequent review
    // 400s with whatever cryptic message the upstream provider
    // returns for `"model": ""`. Surface this at startup instead.
    if reasoning_model.trim().is_empty() {
        anyhow::bail!(
            "LLM_REASONING_MODEL is set to an empty/whitespace value; \
             unset it to take the default (qwen2.5-coder:32b) or set \
             a real model name"
        );
    }

    // Bot identity: read once and validate up-front so the poller
    // and the chat handler see the same values. AR_BOT_LOGIN gates
    // self-loop detection (`is_bot_self`); an empty value would
    // never match any Forgejo sender and the bot would reply to
    // its own comments — a real loop bomb. AR_BOT_NAME is the
    // mention parser's `@<name>` token; an empty value would match
    // every `@` and fire on every PR thread mention, also bad.
    let bot_login = match env::var("AR_BOT_LOGIN") {
        Ok(v) if v.trim().is_empty() => {
            anyhow::bail!(
                "AR_BOT_LOGIN is set to an empty/whitespace value; \
                 unset it to take the default (`auto_review`) or set \
                 the bot's actual Forgejo login"
            );
        }
        Ok(v) => v,
        Err(_) => "auto_review".to_string(),
    };
    let bot_name = match env::var("AR_BOT_NAME") {
        Ok(v) if v.trim().is_empty() => {
            anyhow::bail!(
                "AR_BOT_NAME is set to an empty/whitespace value; \
                 unset it to inherit AR_BOT_LOGIN or set the @-handle \
                 users mention"
            );
        }
        Ok(v) => v,
        Err(_) => bot_login.clone(),
    };

    let forgejo =
        Arc::new(ForgejoClient::new(&forgejo_base, &forgejo_token).context("forgejo client")?);

    let reasoning_provider = Arc::new(
        OpenAiProvider::new(&llm_base, llm_api_key.as_deref(), &reasoning_model)
            .context("reasoning LLM provider")?,
    );
    let mut router = LlmRouter::new().with(ModelTier::Reasoning, reasoning_provider);

    // Optional Embedding tier — when configured, the orchestrator
    // builds a RAG context from the cloned workspace and injects
    // it into the LLM prompt. Reuses LLM_BASE_URL + LLM_API_KEY by
    // default; override with LLM_EMBEDDING_BASE_URL / _API_KEY when
    // your embedder lives on a different endpoint.
    if let Some(embedding_model) = read_non_empty_env("LLM_EMBEDDING_MODEL") {
        let embed_base =
            read_non_empty_env("LLM_EMBEDDING_BASE_URL").unwrap_or_else(|| llm_base.clone());
        let embed_key = read_non_empty_env("LLM_EMBEDDING_API_KEY").or_else(|| llm_api_key.clone());
        let mut provider = OpenAiProvider::new(&embed_base, embed_key.as_deref(), &embedding_model)
            .context("embedding LLM provider")?;
        provider = provider.with_embedding_model(&embedding_model);
        let provider = Arc::new(provider);
        router = router.with(ModelTier::Embedding, provider);
        tracing::info!(model = %embedding_model, "embedding tier configured; RAG enabled");
    } else {
        tracing::info!("LLM_EMBEDDING_MODEL not set; RAG disabled");
    }

    // Optional Cheap tier — used by the LLM-driven file triage step.
    if let Some(cheap_model) = read_non_empty_env("LLM_CHEAP_MODEL") {
        let cheap_base =
            read_non_empty_env("LLM_CHEAP_BASE_URL").unwrap_or_else(|| llm_base.clone());
        let cheap_key = read_non_empty_env("LLM_CHEAP_API_KEY").or_else(|| llm_api_key.clone());
        let provider = Arc::new(
            OpenAiProvider::new(&cheap_base, cheap_key.as_deref(), &cheap_model)
                .context("cheap LLM provider")?,
        );
        router = router.with(ModelTier::Cheap, provider);
        tracing::info!(model = %cheap_model, "cheap tier configured; LLM triage enabled");
    } else {
        tracing::info!("LLM_CHEAP_MODEL not set; LLM triage disabled (heuristic only)");
    }

    let llm_router = Arc::new(router);

    // Single shared learnings store: writes from the chat handler
    // (remember/forget) become visible to RAG retrieval in subsequent
    // reviews. Set AR_LEARNINGS_DB to a filesystem path to persist
    // across restarts; otherwise an in-memory store is used.
    let learnings: Arc<dyn ar_index::LearningsStore> = match read_non_empty_env("AR_LEARNINGS_DB") {
        Some(path) => {
            let path = PathBuf::from(path);
            let store = SqliteLearningsStore::open(&path)
                .await
                .with_context(|| format!("open learnings db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "learnings store: SQLite (persistent)");
            Arc::new(store)
        }
        None => {
            tracing::info!("learnings store: in-memory (volatile across restarts)");
            Arc::new(InMemoryLearningsStore::new())
        }
    };

    let sandbox = build_sandbox()?;

    // Shared review history. Both the orchestrator's incremental-
    // review dedup AND the chat poller need to enumerate the PRs
    // we've reviewed; constructing one Arc and threading it through
    // both keeps them consistent. Set AR_HISTORY_DB to a filesystem
    // path to persist across restarts; otherwise an in-memory store
    // is used (every restart triggers a fresh full review on the
    // next webhook for any open PR).
    let history: Arc<dyn ReviewHistory> = match read_non_empty_env("AR_HISTORY_DB") {
        Some(path) => {
            let path = PathBuf::from(path);
            let store = SqliteReviewHistory::open(&path)
                .await
                .with_context(|| format!("open history db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "review history: SQLite (persistent)");
            Arc::new(store)
        }
        None => {
            tracing::info!("review history: in-memory (volatile across restarts)");
            Arc::new(InMemoryReviewHistory::new())
        }
    };

    let metrics = Arc::new(Metrics::new());
    let observer: Arc<dyn ar_orchestrator::ReviewObserver> =
        Arc::new(MetricsObserver::new(metrics.clone()));

    let mut dispatcher_builder = SpawningDispatcher::new(
        forgejo.clone(),
        llm_router.clone(),
        forgejo_base.clone(),
        forgejo_token.clone(),
    )
    .with_history(history.clone())
    .with_learnings(learnings.clone())
    .with_sandbox(sandbox)
    .with_observer(observer);

    // Optional concurrency cap on in-flight reviews. Without this,
    // a burst of N PRs spawns N tmpdirs + N in-flight LLM calls.
    // For high-traffic instances or expensive cloud LLMs the
    // operator wants a cap; small deployments leave it unset.
    if let Some(max) = parse_env::<usize>("AR_REVIEW_CONCURRENCY") {
        dispatcher_builder = dispatcher_builder.with_concurrency_limit(max);
        tracing::info!(max, "review concurrency cap enabled");
    }

    let dispatcher = Arc::new(dispatcher_builder);

    // Background poller for inline review-thread `@auto_review`
    // mentions. Forgejo doesn't fire pull_request_review_comment
    // webhooks reliably for thread replies (gitea#26023), so we
    // poll. Disabled when AR_POLL_INTERVAL_SECS=0.
    let poll_interval_secs =
        parse_env::<u64>("AR_POLL_INTERVAL_SECS").unwrap_or(DEFAULT_POLL_INTERVAL.as_secs());
    if poll_interval_secs > 0 {
        let dispatcher_dyn: Arc<dyn ar_orchestrator::JobDispatcher> = dispatcher.clone();
        ChatPoller::new(
            forgejo.clone(),
            llm_router.clone(),
            learnings.clone(),
            history.clone(),
            dispatcher_dyn,
            bot_login.clone(),
            bot_name.clone(),
        )
        .with_metrics(metrics.clone())
        .spawn(Duration::from_secs(poll_interval_secs));
        tracing::info!(
            interval_secs = poll_interval_secs,
            bot_login = %bot_login,
            bot_name = %bot_name,
            "chat poller running"
        );
    } else {
        tracing::info!("AR_POLL_INTERVAL_SECS=0; chat poller disabled");
    }

    let chat_deps = ChatDeps {
        forgejo: forgejo.clone(),
        llm: llm_router.clone(),
        learnings,
    };

    // Wire the readiness probe to the same Forgejo client the chat
    // handler uses. The TTL (default 10s) is tuneable via env so
    // operators with aggressive k8s probe schedules can lengthen it
    // to avoid hammering Forgejo.
    let readiness_ttl_secs = parse_env::<u64>("AR_READINESS_TTL_SECS").unwrap_or(10);
    let readiness = Arc::new(ReadinessProbe::with_ttl(
        forgejo.clone(),
        Duration::from_secs(readiness_ttl_secs),
    ));

    // Snapshot the runtime config for /info. Read env-var-driven
    // booleans here once rather than threading them through every
    // builder call.
    let info = Arc::new(GatewayInfo {
        name: "auto_review",
        version: env!("CARGO_PKG_VERSION"),
        bot_login: bot_login.clone(),
        bot_name: bot_name.clone(),
        sandbox: if read_non_empty_env("AR_SANDBOX_IMAGE").is_some() {
            "podman"
        } else {
            "direct"
        },
        learnings: if read_non_empty_env("AR_LEARNINGS_DB").is_some() {
            "sqlite"
        } else {
            "in-memory"
        },
        history: if read_non_empty_env("AR_HISTORY_DB").is_some() {
            "sqlite"
        } else {
            "in-memory"
        },
        llm_tiers: {
            let mut tiers = vec!["reasoning"]; // always present (required)
            if read_non_empty_env("LLM_CHEAP_MODEL").is_some() {
                tiers.push("cheap");
            }
            if read_non_empty_env("LLM_EMBEDDING_MODEL").is_some() {
                tiers.push("embedding");
            }
            tiers
        },
        reasoning_model: reasoning_model.clone(),
        poller_enabled: poll_interval_secs > 0,
        readiness_enabled: true,
    });

    let mut state = AppState::new(secret, dispatcher)
        .with_chat(chat_deps)
        .with_bot_identity(bot_login, bot_name)
        .with_metrics(metrics)
        .with_readiness(readiness)
        .with_info(info);

    // Webhook delivery dedup. On by default with a 256-ID LRU;
    // operators can override via AR_DEDUP_CAPACITY (set to 0 to
    // disable, e.g. for tests that want every delivery dispatched).
    let dedup_capacity = parse_env::<usize>("AR_DEDUP_CAPACITY").unwrap_or(256);
    if dedup_capacity > 0 {
        let dedup = Arc::new(RecentDeliveries::new(dedup_capacity));
        state = state.with_webhook_dedup(dedup);
        tracing::info!(capacity = dedup_capacity, "webhook delivery dedup enabled");
    }

    // Optional global webhook throttle (T7 mitigation). Off by
    // default so existing deployments don't suddenly start
    // shedding traffic; operators opt in by setting both env
    // vars. The intended values for a self-host fronting a single
    // Forgejo instance are tens of req/s and a burst around 30 —
    // legitimate Forgejo traffic is well under that.
    let rate_per_sec = parse_env::<u32>("AR_WEBHOOK_RATE_PER_SEC");
    let burst = parse_env::<u32>("AR_WEBHOOK_BURST");
    match (rate_per_sec, burst) {
        (Some(rate), Some(burst)) => {
            let bucket = Arc::new(TokenBucket::new(burst, rate));
            state = state.with_webhook_rate_limit(bucket);
            tracing::info!(rate, burst, "webhook rate limiter enabled");
        }
        // Only one half set — operator probably meant to enable
        // the limiter but missed the partner var. Without this
        // warning the rate limit is silently off and the operator
        // discovers it during an incident.
        (Some(_), None) => {
            tracing::warn!(
                "AR_WEBHOOK_RATE_PER_SEC is set but AR_WEBHOOK_BURST is not; \
                 rate limiter requires both — DISABLED. Set both or unset both."
            );
        }
        (None, Some(_)) => {
            tracing::warn!(
                "AR_WEBHOOK_BURST is set but AR_WEBHOOK_RATE_PER_SEC is not; \
                 rate limiter requires both — DISABLED. Set both or unset both."
            );
        }
        (None, None) => {} // intentional: no rate limit
    }

    let app = build_router(state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, "ar-gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("graceful shutdown complete");
    Ok(())
}

/// Shutdown signal handler. Returns when SIGTERM (Unix) or
/// SIGINT (Ctrl-C, cross-platform) arrives. Used as the
/// `with_graceful_shutdown` argument on `axum::serve` so:
/// - in-flight HTTP responses finish cleanly,
/// - the listener stops accepting new connections immediately,
/// - the process exits 0 once the listener drains.
///
/// Note: review tasks the dispatcher has already `tokio::spawn`-ed
/// continue running after the listener drains, since they're not
/// joined on. The tokio runtime drops them when `main` returns.
/// This is best-effort by design — adding a join set across the
/// dispatcher boundary would mean threading a CancellationToken
/// through every spawned activity, which is more machinery than
/// the single-tenant deploy needs. Operators wanting zero data
/// loss should drain via the systemd `ExecStop=` hook with a
/// short pre-stop sleep before SIGTERM, so in-flight reviews
/// reach their commit-status post.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "ctrl_c handler failed; shutdown trigger disabled");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "SIGTERM handler init failed");
                    return;
                }
            };
        term.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received SIGINT; draining listener");
        }
        _ = terminate => {
            tracing::info!("received SIGTERM; draining listener");
        }
    }
}

/// Choose a sandbox based on env vars. Setting `AR_SANDBOX_IMAGE`
/// switches the gateway from the unsafe direct-spawn path to a
/// hardened [`PodmanSandbox`] that wraps every linter invocation
/// in `podman run --network=none --read-only ...`.
///
/// Without `AR_SANDBOX_IMAGE`, linter binaries spawn directly on the
/// host. That's fine for local dev but exposes the operator to the
/// Kudelski-class RCE risk for any internet-facing deploy.
/// Read an env var, treating both "unset" and "empty / whitespace-only"
/// as `None`. Most operator-facing env vars take a meaningful default
/// when unset; an explicit empty assignment (`FOO=`) is almost always
/// a misconfiguration that should fall through to the same default
/// rather than silently producing a broken empty string.
fn read_non_empty_env(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if v.trim().is_empty() => {
            tracing::warn!(
                env = name,
                "env var set to an empty/whitespace value; treating as unset"
            );
            None
        }
        Ok(v) => Some(v),
        Err(_) => None,
    }
}

/// Parse an env var as an integer, distinguishing "unset" from
/// "set but unparseable". The previous `.parse::<X>().ok()` pattern
/// silently swallowed garbage values like `AR_REVIEW_CONCURRENCY=ten`,
/// leaving the operator with no signal that their config didn't take
/// effect. This warn-and-fall-through variant surfaces the typo.
fn parse_env<T>(name: &str) -> Option<T>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    let raw = read_non_empty_env(name)?;
    match raw.parse::<T>() {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                env = name,
                value = %raw,
                error = %e,
                "env var set to an unparseable value; using the built-in default"
            );
            None
        }
    }
}

fn build_sandbox() -> Result<Arc<dyn Sandbox>> {
    if let Some(image) = read_non_empty_env("AR_SANDBOX_IMAGE") {
        let memory_mib = parse_env::<u64>("AR_SANDBOX_MEMORY_MIB").unwrap_or(512);
        let cpus = parse_env::<f64>("AR_SANDBOX_CPUS").unwrap_or(1.0);
        let pids_limit = parse_env::<u32>("AR_SANDBOX_PIDS_LIMIT").unwrap_or(128);
        let wall_clock_secs = parse_env::<u64>("AR_SANDBOX_TIMEOUT_SECS").unwrap_or(60);
        let podman_bin = env::var("AR_SANDBOX_PODMAN_BIN").unwrap_or_else(|_| "podman".into());
        let cfg = PodmanSandboxConfig {
            image: image.clone(),
            memory_mib,
            cpus,
            pids_limit,
            wall_clock: Duration::from_secs(wall_clock_secs),
            podman_bin,
        };
        tracing::info!(
            image,
            memory_mib,
            cpus,
            pids_limit,
            wall_clock_secs,
            "sandbox: podman (hardened)"
        );
        Ok(Arc::new(PodmanSandbox::new(cfg)))
    } else {
        tracing::warn!(
            "sandbox: direct (NO ISOLATION). Set AR_SANDBOX_IMAGE for production deploys."
        );
        Ok(Arc::new(DirectSandbox::new()))
    }
}
