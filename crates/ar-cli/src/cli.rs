use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "auto_review",
    version,
    about = "Operator CLI for the auto_review Forgejo bot."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Mint a personal access token for the auto_review bot user.
    ///
    /// Uses the bot user's own password (HTTP Basic) to create a PAT with
    /// the scopes the reviewer needs. The PAT is printed once; save it
    /// into FORGEJO_TOKEN.
    Init(InitArgs),

    /// Register a webhook on a repository so PR events flow to the
    /// reviewer.
    RegisterWebhook(RegisterWebhookArgs),

    /// Run the full review pipeline once against a specific PR. No
    /// gateway or webhook required — useful for development, demos, and
    /// reproducing reported issues.
    ReviewOnce(ReviewOnceArgs),

    /// Replay one or more PR fixtures through the LLM-review step
    /// without touching Forgejo. Reports per-fixture latency, finding
    /// counts, and self-heal attempts; aggregates over the batch.
    /// Useful for picking models, tuning prompts, and tracking
    /// regression in review behaviour over time.
    Bench(BenchArgs),

    /// Validate one or more `.auto_review.yaml` configuration files.
    /// Parses each file with the same code path the gateway uses and
    /// surfaces any errors with line numbers. Exits non-zero on
    /// validation failure so this fits cleanly in a pre-commit hook
    /// or CI step.
    ValidateConfig(ValidateConfigArgs),
}

#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Base URL of your Forgejo instance, e.g. https://git.example.com.
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: String,

    /// The bot account's username.
    #[arg(long)]
    pub username: String,

    /// The bot account's password. If omitted, prompts on stdin.
    #[arg(long)]
    pub password: Option<String>,

    /// Name to give the new token.
    #[arg(long, default_value = "auto_review")]
    pub token_name: String,

    /// Scopes to grant the new token (defaults are the minimum needed
    /// for review posting + webhook registration).
    #[arg(long, value_delimiter = ',', default_values_t = default_scopes())]
    pub scopes: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct ReviewOnceArgs {
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: String,

    #[arg(long, env = "FORGEJO_TOKEN")]
    pub token: String,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Pull-request number.
    #[arg(long)]
    pub pr: u64,

    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: String,

    #[arg(long, env = "LLM_API_KEY")]
    pub llm_api_key: Option<String>,

    #[arg(long, env = "LLM_REASONING_MODEL", default_value = "qwen2.5-coder:32b")]
    pub llm_model: String,

    /// Print the rendered LLM prompt and exit. Skips clone, lint, LLM,
    /// and posting. Useful for tuning .auto_review.yaml or debugging
    /// prompt content without burning tokens or touching the PR.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::Args, Debug)]
pub struct ValidateConfigArgs {
    /// One or more `.auto_review.yaml` paths (or directories
    /// containing such files).
    #[arg(required = true)]
    pub paths: Vec<std::path::PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    /// One or more fixture file paths or directories. Each fixture is
    /// a JSON file with the shape documented in `bench/README.md`.
    /// When a directory is given, every `*.json` file in it is loaded.
    #[arg(required = true)]
    pub fixtures: Vec<std::path::PathBuf>,

    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: String,

    #[arg(long, env = "LLM_API_KEY")]
    pub llm_api_key: Option<String>,

    #[arg(long, env = "LLM_REASONING_MODEL", default_value = "qwen2.5-coder:32b")]
    pub llm_model: String,

    /// Optional cheap-tier model. When set, runs the verifier pass
    /// after the reasoning model and reports findings before/after.
    #[arg(long, env = "LLM_CHEAP_MODEL")]
    pub llm_cheap_model: Option<String>,

    /// Print the final aggregate as one line of JSON instead of the
    /// human-readable table. Lets you pipe runs into a regression
    /// tracker.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct RegisterWebhookArgs {
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: String,

    #[arg(long, env = "FORGEJO_TOKEN")]
    pub token: String,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Public URL the gateway is reachable at (the path
    /// `/webhooks/forgejo` is appended automatically).
    #[arg(long)]
    pub gateway_url: String,

    /// Webhook secret. Must match the gateway's WEBHOOK_SECRET.
    #[arg(long, env = "WEBHOOK_SECRET")]
    pub webhook_secret: String,
}

fn default_scopes() -> Vec<String> {
    vec![
        "write:repository".into(),
        "write:issue".into(),
        "read:user".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn init_parses_minimum_args() {
        let cli = Cli::try_parse_from([
            "auto_review",
            "init",
            "--forgejo-url",
            "https://x.example",
            "--username",
            "bot",
        ])
        .expect("parse");
        match cli.command {
            Command::Init(a) => {
                assert_eq!(a.forgejo_url, "https://x.example");
                assert_eq!(a.username, "bot");
                assert!(a.password.is_none());
                assert_eq!(a.token_name, "auto_review");
                assert!(!a.scopes.is_empty());
            }
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn init_accepts_password_and_custom_scopes() {
        let cli = Cli::try_parse_from([
            "auto_review",
            "init",
            "--forgejo-url",
            "https://x",
            "--username",
            "bot",
            "--password",
            "p",
            "--scopes",
            "write:repository,read:user",
        ])
        .expect("parse");
        match cli.command {
            Command::Init(a) => {
                assert_eq!(a.password.as_deref(), Some("p"));
                assert_eq!(a.scopes, vec!["write:repository", "read:user"]);
            }
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn register_webhook_requires_all_args() {
        let cli = Cli::try_parse_from([
            "auto_review",
            "register-webhook",
            "--forgejo-url",
            "https://x",
            "--token",
            "tok",
            "--owner",
            "o",
            "--repo",
            "r",
            "--gateway-url",
            "https://g.example",
            "--webhook-secret",
            "s",
        ])
        .expect("parse");
        match cli.command {
            Command::RegisterWebhook(a) => {
                assert_eq!(a.owner, "o");
                assert_eq!(a.repo, "r");
                assert_eq!(a.gateway_url, "https://g.example");
            }
            _ => panic!("expected RegisterWebhook"),
        }
    }

    #[test]
    fn review_once_parses_required_args() {
        let cli = Cli::try_parse_from([
            "auto_review",
            "review-once",
            "--forgejo-url",
            "https://x",
            "--token",
            "tok",
            "--owner",
            "alice",
            "--repo",
            "widgets",
            "--pr",
            "42",
            "--llm-base-url",
            "http://localhost:11434",
        ])
        .expect("parse");
        match cli.command {
            Command::ReviewOnce(a) => {
                assert_eq!(a.owner, "alice");
                assert_eq!(a.repo, "widgets");
                assert_eq!(a.pr, 42);
                assert_eq!(a.llm_base_url, "http://localhost:11434");
                assert_eq!(a.llm_model, "qwen2.5-coder:32b");
                assert!(a.llm_api_key.is_none());
            }
            _ => panic!("expected ReviewOnce"),
        }
    }

    #[test]
    fn validate_config_accepts_multiple_paths() {
        let cli = Cli::try_parse_from([
            "auto_review",
            "validate-config",
            "/tmp/a/.auto_review.yaml",
            "/tmp/b",
        ])
        .expect("parse");
        match cli.command {
            Command::ValidateConfig(a) => {
                assert_eq!(a.paths.len(), 2);
            }
            _ => panic!("expected ValidateConfig"),
        }
    }

    #[test]
    fn validate_config_requires_at_least_one_path() {
        let res = Cli::try_parse_from(["auto_review", "validate-config"]);
        assert!(res.is_err());
    }

    #[test]
    fn missing_required_arg_is_an_error() {
        let res = Cli::try_parse_from([
            "auto_review",
            "init",
            "--forgejo-url",
            "https://x",
            // username missing
        ]);
        assert!(res.is_err());
    }
}
