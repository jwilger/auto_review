use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::{build_router, AppState};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::SpawningDispatcher;
use std::env;
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
    let llm_router = Arc::new(LlmRouter::new().with(ModelTier::Reasoning, reasoning_provider));

    let dispatcher = Arc::new(SpawningDispatcher::new(
        forgejo,
        llm_router,
        forgejo_base.clone(),
        forgejo_token.clone(),
    ));
    let state = AppState::new(secret, dispatcher);
    let app = build_router(state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, "ar-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}
