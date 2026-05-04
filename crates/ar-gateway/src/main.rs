use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::config::{compose_state_path, resolve_db_backing, DbBacking};
use ar_gateway::dedup::{DeliveryDedup, RecentDeliveries, SqliteDeliveries};
use ar_gateway::metrics::{Metrics, MetricsObserver};
use ar_gateway::poller::{ChatPoller, SharedCommentCursors, DEFAULT_POLL_INTERVAL};
use ar_gateway::ratelimit::TokenBucket;
use ar_gateway::{build_router, AppState, ChatDeps, GatewayInfo, ReadinessProbe};
use ar_index::{
    InMemoryLearningsStore, InMemoryVectorStore, SqliteLearningsStore, SqliteVectorStore,
    VectorStore,
};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::review_history::{InMemoryReviewHistory, ReviewHistory};
use ar_orchestrator::sqlite_history::SqliteReviewHistory;
use ar_orchestrator::SpawningDispatcher;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ar_gateway=debug")),
        )
        .init();

    let bind = env::var("AR_GATEWAY_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    // git is required for the workspace clone phase. Probe up
    // front so a missing-git deploy surfaces in the first log
    // scrape rather than the first failed review's opaque
    // "No such file or directory" io error. Don't bail — the
    // gateway should still serve /healthz and /metrics for
    // operators investigating; reviews just fail loudly per-PR.
    match tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            tracing::info!(
                version = %String::from_utf8_lossy(&out.stdout).trim(),
                "git OK"
            );
        }
        Ok(out) => {
            tracing::warn!(
                status = %out.status,
                "git --version exited non-zero; reviews will fail at the clone phase"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "git not found in PATH; every review will fail at prepare_workspace. \
                 Install git or add it to PATH."
            );
        }
    }

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
    let forgejo_token = forgejo_api_token_from_env_values(read_non_empty_env("AR_FORGEJO_TOKEN"))?;
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
        // For Ollama-backed embedders, explicitly send options.num_ctx
        // so a bigger byte cap doesn't get silently truncated by the
        // server's default 2048. Ignored by hosted OpenAI.
        if let Some(num_ctx) = parse_env::<u32>("AR_EMBED_NUM_CTX") {
            provider = provider.with_embed_num_ctx(num_ctx);
            tracing::info!(num_ctx, "embedding num_ctx override enabled");
        }
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
    // reviews. Persistent SQLite by default (at the per-store XDG path);
    // operators opt out with `AR_LEARNINGS_DB=:memory:` or override
    // the path with `AR_LEARNINGS_DB=/path/to/learnings.db`.
    let learnings_backing = resolve_db_backing(
        env::var("AR_LEARNINGS_DB").ok().as_deref(),
        &default_state_path("learnings.db"),
    );
    let (learnings, learnings_info) = match &learnings_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for learnings db at {}", path.display())
            })?;
            let store = SqliteLearningsStore::open(path)
                .await
                .with_context(|| format!("open learnings db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "learnings store: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn ar_index::LearningsStore>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("learnings store: in-memory (AR_LEARNINGS_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryLearningsStore::new()) as Arc<dyn ar_index::LearningsStore>,
                "in-memory".to_string(),
            )
        }
    };

    // Shared review history. Both the orchestrator's incremental-
    // review dedup AND the chat poller need to enumerate the PRs
    // we've reviewed; constructing one Arc and threading it through
    // both keeps them consistent. Persistent SQLite by default;
    // `AR_HISTORY_DB=:memory:` opts out (every restart triggers a
    // fresh full review on the next webhook for any open PR).
    let history_backing = resolve_db_backing(
        env::var("AR_HISTORY_DB").ok().as_deref(),
        &default_state_path("history.db"),
    );
    let (history, history_info) = match &history_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for history db at {}", path.display())
            })?;
            let store = SqliteReviewHistory::open(path)
                .await
                .with_context(|| format!("open history db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "review history: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn ReviewHistory>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("review history: in-memory (AR_HISTORY_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryReviewHistory::new()) as Arc<dyn ReviewHistory>,
                "in-memory".to_string(),
            )
        }
    };

    // Shared symbol-embedding store. Persistent SQLite by default
    // so symbol embeddings survive across reviews (and across gateway
    // restarts). `AR_VECTOR_DB=:memory:` opts out — useful for tests
    // and ephemeral previews where re-embedding on each review is
    // acceptable. The wins matter most for the slow local Ollama
    // embedder; hosted OpenAI is fast enough that operators may not
    // bother with persistence.
    let vector_backing = resolve_db_backing(
        env::var("AR_VECTOR_DB").ok().as_deref(),
        &default_state_path("vector.db"),
    );
    let (vector_store, vector_info) = match &vector_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for vector db at {}", path.display())
            })?;
            let store = SqliteVectorStore::open(path)
                .await
                .with_context(|| format!("open vector db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "vector store: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn VectorStore>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("vector store: in-memory (AR_VECTOR_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryVectorStore::new()) as Arc<dyn VectorStore>,
                "in-memory".to_string(),
            )
        }
    };

    // Webhook delivery dedup. Persistent SQLite by default; operators
    // opt out of persistence with `AR_DEDUP_DB=:memory:` (in-memory
    // LRU bounded by `AR_DEDUP_CAPACITY`, default 256), or disable
    // dedup entirely with `AR_DEDUP_CAPACITY=0` (mostly for tests
    // that want every well-signed delivery dispatched). Computed
    // upfront so the chosen backing lands in /info alongside the
    // others, even though the actual `with_webhook_dedup` call
    // happens further down once `state` exists.
    let dedup_capacity = parse_env::<usize>("AR_DEDUP_CAPACITY").unwrap_or(256);
    let dedup_backing = resolve_db_backing(
        env::var("AR_DEDUP_DB").ok().as_deref(),
        &default_state_path("dedup.db"),
    );
    let (dedup_store, dedup_info): (Option<Arc<dyn DeliveryDedup>>, String) = if dedup_capacity == 0
    {
        tracing::info!("webhook delivery dedup: disabled (AR_DEDUP_CAPACITY=0)");
        (None, "disabled".into())
    } else {
        match &dedup_backing {
            DbBacking::Sqlite(path) => {
                ensure_parent_dir(path).with_context(|| {
                    format!("create parent dir for dedup db at {}", path.display())
                })?;
                let store = SqliteDeliveries::open(path)
                    .await
                    .with_context(|| format!("open dedup db at {}", path.display()))?;
                tracing::info!(path = %path.display(), "webhook delivery dedup: SQLite (persistent)");
                (
                    Some(Arc::new(store) as Arc<dyn DeliveryDedup>),
                    format!("sqlite:{}", path.display()),
                )
            }
            DbBacking::InMemory => {
                let store = RecentDeliveries::new(dedup_capacity);
                tracing::info!(
                    capacity = dedup_capacity,
                    "webhook delivery dedup: in-memory LRU (AR_DEDUP_DB=:memory: opt-out)"
                );
                (
                    Some(Arc::new(store) as Arc<dyn DeliveryDedup>),
                    format!("in-memory(capacity={dedup_capacity})"),
                )
            }
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
    .with_vector_store(vector_store.clone())
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
    let chat_comment_cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::new()));
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
        .with_cursors(chat_comment_cursors.clone())
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
        learnings: learnings_info.clone(),
        history: history_info.clone(),
        vector: vector_info.clone(),
        dedup: dedup_info.clone(),
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
        .with_chat_comment_cursors(chat_comment_cursors)
        .with_info(info);

    if let Some(action_token) = validate_ci_review_token(read_non_empty_env("AR_CI_REVIEW_TOKEN"))?
    {
        state = state.with_ci_review_endpoint(action_token, forgejo.clone());
        tracing::info!("CI review endpoint enabled at POST /reviews/ci");
    } else {
        tracing::info!("CI review endpoint disabled (AR_CI_REVIEW_TOKEN unset)");
    }

    if let Some(dedup) = dedup_store {
        state = state.with_webhook_dedup(dedup);
    }

    // Single-line summary of the four persistence backings, so an
    // operator can confirm at startup which file the bot opened
    // (or that everything's volatile) without diffing four
    // separate lines above.
    tracing::info!(
        learnings = %learnings_info,
        history = %history_info,
        vector = %vector_info,
        dedup = %dedup_info,
        "persistence backings selected",
    );

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

/// Read an env var, treating both "unset" and "empty / whitespace-only"
/// as `None`. Most operator-facing env vars take a meaningful default
/// when unset; an explicit empty assignment (`FOO=`) is almost always
/// a misconfiguration that should fall through to the same default
/// rather than silently producing a broken empty string.
/// Compute the per-store XDG default sqlite path for `filename`. Thin
/// wrapper around the pure [`compose_state_path`] so unit tests don't
/// have to mutate the process env.
fn default_state_path(filename: &str) -> PathBuf {
    let xdg = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    compose_state_path(xdg.as_deref(), home.as_deref(), filename)
}

/// Create the parent directory of `path` if it doesn't exist. The
/// SQLite stores' `open()` would otherwise fail with a confusing
/// "unable to open database file" on first run when the XDG state
/// dir is missing. `create_dir_all` is idempotent.
fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

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

fn forgejo_api_token_from_env_values(ar_forgejo_token: Option<String>) -> Result<String> {
    ar_forgejo_token.context("AR_FORGEJO_TOKEN is required")
}

fn validate_ci_review_token(raw: Option<String>) -> Result<Option<String>> {
    let Some(token) = raw else {
        return Ok(None);
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(None);
    }
    if token.len() < 32 {
        anyhow::bail!(
            "AR_CI_REVIEW_TOKEN is too short; configure a strong token of at least 32 characters"
        );
    }
    Ok(Some(token))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_review_token_unset_empty_or_whitespace_disables_endpoint() {
        for raw in [None, Some(""), Some("   \t\n  ")] {
            let validated = validate_ci_review_token(raw.map(str::to_string));

            assert!(
                matches!(validated, Ok(None)),
                "expected {raw:?} to disable the CI review endpoint, got {validated:?}"
            );
        }
    }

    #[test]
    fn ci_review_token_accepts_strong_random_value() {
        let token = "0123456789abcdef0123456789abcdef".to_string();

        let validated = validate_ci_review_token(Some(token.clone()));

        assert_eq!(validated.unwrap(), Some(token));
    }

    #[test]
    fn ci_review_token_rejects_short_non_empty_value() {
        let rejected_token = "abc123-token-value";
        let err = validate_ci_review_token(Some(rejected_token.to_string())).unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("AR_CI_REVIEW_TOKEN"),
            "error should name AR_CI_REVIEW_TOKEN, got: {message}"
        );
        assert!(
            message.contains("too short") || message.contains("strong token"),
            "error should explain the token is too short or needs a strong token, got: {message}"
        );
        assert!(
            !message.contains(rejected_token),
            "error must not echo the rejected token value, got: {message}"
        );
    }

    #[test]
    fn forgejo_api_token_accepts_gateway_bot_env() {
        let gateway_bot_token = "gateway-bot-pat".to_string();

        let token = forgejo_api_token_from_env_values(Some(gateway_bot_token.clone())).unwrap();

        assert_eq!(token, gateway_bot_token);
    }

    #[test]
    fn forgejo_api_token_requires_gateway_bot_env() {
        let err = forgejo_api_token_from_env_values(None).unwrap_err();

        assert!(
            err.to_string().contains("AR_FORGEJO_TOKEN"),
            "missing gateway bot token error should name AR_FORGEJO_TOKEN, got: {err}"
        );
    }
}
