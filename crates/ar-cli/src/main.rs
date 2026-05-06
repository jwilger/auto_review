use anyhow::Result;
use clap::Parser;

mod bench;
mod cli;
mod commands;

use cli::{
    AuthCommand, BenchCommand, Cli, Command, ConfigCommand, HistoryCommand, LearningsCommand,
    OpsCommand, ReviewCommand, WebhookCommand,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Command::Gateway(args) = cli.command {
        return ar_gateway::run_from_env(ar_gateway::StartupOptions { bare: args.bare }).await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match cli.command {
        Command::Gateway(_) => unreachable!("gateway command returned before CLI tracing init"),
        Command::Auth(AuthCommand::Init(args)) => commands::init(args).await,
        Command::Webhook(WebhookCommand::Register(args)) => commands::register_webhook(args).await,
        Command::Webhook(WebhookCommand::List(args)) => commands::list_webhooks(args).await,
        Command::Webhook(WebhookCommand::Unregister(args)) => {
            commands::unregister_webhook(args).await
        }
        Command::Webhook(WebhookCommand::Test(args)) => commands::test_webhook(args).await,
        Command::Config(ConfigCommand::Validate(args)) => commands::validate_config(args),
        Command::Review(ReviewCommand::Once(args)) => commands::review_once(args).await,
        Command::Bench(BenchCommand::Run(args)) => bench::run(args).await,
        Command::Ops(OpsCommand::Doctor(args)) => commands::doctor(args).await,
        Command::Ops(OpsCommand::Status(args)) => commands::status(args).await,
        Command::History(HistoryCommand::ResetPr(args)) => commands::reset_pr(args).await,
        Command::History(HistoryCommand::Purge(args)) => commands::purge_history(args).await,
        Command::Learnings(LearningsCommand::List(args)) => commands::list_learnings(args).await,
        Command::Learnings(LearningsCommand::Forget(args)) => commands::forget_learning(args).await,
    }
}
