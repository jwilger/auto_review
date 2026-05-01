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
