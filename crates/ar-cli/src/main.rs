use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    println!("auto_review CLI (skeleton). Subcommands: init, register-webhook, replay, run-once.");
    Ok(())
}
