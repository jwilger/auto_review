use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::metrics::{Metrics, MetricsObserver};
use ar_gateway::poller::{ChatPoller, DEFAULT_POLL_INTERVAL};
use ar_gateway::{build_router, AppState, ChatDeps, GatewayInfo, ReadinessProbe};
use ar_index::{InMemoryLearningsStore, SqliteLearningsStore};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::review_history::{InMemoryReviewHistory, ReviewHistory};
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
    let forgejo_base = env::var("FORGEJO_BASE_URL").context("FORGEJO_BASE_URL is required")?;
    let forgejo_token = env::var("FORGEJO_TOKEN").context("FORGEJO_TOKEN is required")?;
    let llm_base = env::var("LLM_BASE_URL").context("LLM_BASE_URL is required")?;
    let llm_api_key = env::var("LLM_API_KEY").ok();
    let reasoning_model =
        env::var("LLM_REASONING_MODEL").unwrap_or_else(|_| "qwen2.5-coder:32b".into());

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
    if let Ok(embedding_model) = env::var("LLM_EMBEDDING_MODEL") {
        let embed_base = env::var("LLM_EMBEDDING_BASE_URL").unwrap_or_else(|_| llm_base.clone());
        let embed_key = env::var("LLM_EMBEDDING_API_KEY")
            .ok()
            .or_else(|| llm_api_key.clone());
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
    if let Ok(cheap_model) = env::var("LLM_CHEAP_MODEL") {
        let cheap_base = env::var("LLM_CHEAP_BASE_URL").unwrap_or_else(|_| llm_base.clone());
        let cheap_key = env::var("LLM_CHEAP_API_KEY")
            .ok()
            .or_else(|| llm_api_key.clone());
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
    let learnings: Arc<dyn ar_index::LearningsStore> = match env::var("AR_LEARNINGS_DB").ok() {
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
    // both keeps them consistent.
    let history: Arc<dyn ReviewHistory> = Arc::new(InMemoryReviewHistory::new());

    let metrics = Arc::new(Metrics::new());
    let observer: Arc<dyn ar_orchestrator::ReviewObserver> =
        Arc::new(MetricsObserver::new(metrics.clone()));

    let dispatcher = Arc::new(
        SpawningDispatcher::new(
            forgejo.clone(),
            llm_router.clone(),
            forgejo_base.clone(),
            forgejo_token.clone(),
        )
        .with_history(history.clone())
        .with_learnings(learnings.clone())
        .with_sandbox(sandbox)
        .with_observer(observer),
    );

    // Background poller for inline review-thread `@auto_review`
    // mentions. Forgejo doesn't fire pull_request_review_comment
    // webhooks reliably for thread replies (gitea#26023), so we
    // poll. Disabled when AR_POLL_INTERVAL_SECS=0.
    let poll_interval_secs = env::var("AR_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_INTERVAL.as_secs());
    if poll_interval_secs > 0 {
        let bot_login = env::var("AR_BOT_LOGIN").unwrap_or_else(|_| "auto_review".into());
        let bot_name = env::var("AR_BOT_NAME").unwrap_or_else(|_| bot_login.clone());
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
            bot_login,
            bot_name,
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

    // Same bot identity used by the poller above. Falls back to
    // `auto_review` when the operator hasn't customised it.
    let bot_login = env::var("AR_BOT_LOGIN").unwrap_or_else(|_| "auto_review".into());
    let bot_name = env::var("AR_BOT_NAME").unwrap_or_else(|_| bot_login.clone());

    // Wire the readiness probe to the same Forgejo client the chat
    // handler uses. The TTL (default 10s) is tuneable via env so
    // operators with aggressive k8s probe schedules can lengthen it
    // to avoid hammering Forgejo.
    let readiness_ttl_secs = env::var("AR_READINESS_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
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
        sandbox: if env::var("AR_SANDBOX_IMAGE").is_ok() {
            "podman"
        } else {
            "direct"
        },
        learnings: if env::var("AR_LEARNINGS_DB").is_ok() {
            "sqlite"
        } else {
            "in-memory"
        },
        llm_tiers: {
            let mut tiers = vec!["reasoning"]; // always present (required)
            if env::var("LLM_CHEAP_MODEL").is_ok() {
                tiers.push("cheap");
            }
            if env::var("LLM_EMBEDDING_MODEL").is_ok() {
                tiers.push("embedding");
            }
            tiers
        },
        reasoning_model: reasoning_model.clone(),
        poller_enabled: poll_interval_secs > 0,
        readiness_enabled: true,
    });

    let state = AppState::new(secret, dispatcher)
        .with_chat(chat_deps)
        .with_bot_identity(bot_login, bot_name)
        .with_metrics(metrics)
        .with_readiness(readiness)
        .with_info(info);
    let app = build_router(state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, "ar-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Choose a sandbox based on env vars. Setting `AR_SANDBOX_IMAGE`
/// switches the gateway from the unsafe direct-spawn path to a
/// hardened [`PodmanSandbox`] that wraps every linter invocation
/// in `podman run --network=none --read-only ...`.
///
/// Without `AR_SANDBOX_IMAGE`, linter binaries spawn directly on the
/// host. That's fine for local dev but exposes the operator to the
/// Kudelski-class RCE risk for any internet-facing deploy.
fn build_sandbox() -> Result<Arc<dyn Sandbox>> {
    if let Ok(image) = env::var("AR_SANDBOX_IMAGE") {
        let memory_mib = env::var("AR_SANDBOX_MEMORY_MIB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512);
        let cpus = env::var("AR_SANDBOX_CPUS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);
        let pids_limit = env::var("AR_SANDBOX_PIDS_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(128);
        let wall_clock_secs = env::var("AR_SANDBOX_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
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
