use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::{build_router, AppState, ChatDeps};
use ar_index::{InMemoryLearningsStore, SqliteLearningsStore};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::SpawningDispatcher;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
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

    let dispatcher = Arc::new(
        SpawningDispatcher::new(
            forgejo.clone(),
            llm_router.clone(),
            forgejo_base.clone(),
            forgejo_token.clone(),
        )
        .with_learnings(learnings.clone()),
    );

    let chat_deps = ChatDeps {
        forgejo: forgejo.clone(),
        llm: llm_router.clone(),
        learnings,
    };

    let state = AppState::new(secret, dispatcher).with_chat(chat_deps);
    let app = build_router(state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, "ar-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}
