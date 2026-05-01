use anyhow::Result;
use clap::Parser;

mod bench;
mod cli;
mod commands;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => commands::init(args).await,
        Command::RegisterWebhook(args) => commands::register_webhook(args).await,
        Command::ListWebhooks(args) => commands::list_webhooks(args).await,
        Command::UnregisterWebhook(args) => commands::unregister_webhook(args).await,
        Command::ReviewOnce(args) => commands::review_once(args).await,
        Command::Bench(args) => bench::run(args).await,
        Command::ValidateConfig(args) => commands::validate_config(args),
        Command::ListLinters(args) => commands::list_linters(args),
        Command::TestWebhook(args) => commands::test_webhook(args).await,
        Command::Doctor(args) => commands::doctor(args).await,
        Command::Status(args) => commands::status(args).await,
        Command::ResetPr(args) => commands::reset_pr(args).await,
    }
}
