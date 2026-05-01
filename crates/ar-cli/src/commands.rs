//! Implementations of the CLI subcommands.

use crate::cli::{InitArgs, RegisterWebhookArgs};
use anyhow::{Context, Result};
use ar_forgejo::{
    Client, CreateAccessTokenRequest, CreateWebhookRequest, InitClient, WebhookConfig,
};

const WEBHOOK_PATH: &str = "/webhooks/forgejo";

pub async fn init(args: InitArgs) -> Result<()> {
    let password = match args.password {
        Some(p) => p,
        None => rpassword::prompt_password(format!("Password for {}: ", args.username))
            .context("read password")?,
    };

    let client = InitClient::new(&args.forgejo_url, &args.username, &password)
        .context("build init client")?;
    let request = CreateAccessTokenRequest {
        name: args.token_name,
        scopes: args.scopes,
    };
    let token = client
        .create_access_token(&args.username, &request)
        .await
        .context("create access token")?;

    println!("Created access token {}: {}", token.name, token.id);
    println!("Scopes: {}", token.scopes.join(", "));
    println!();
    println!("Save this token (it will not be shown again):");
    println!();
    println!("    export FORGEJO_TOKEN={}", token.sha1);
    println!();
    println!("Recommended next step:");
    println!("    auto_review register-webhook --owner OWNER --repo REPO \\");
    println!("        --forgejo-url {} \\", args.forgejo_url);
    println!("        --gateway-url https://reviewer.example.com \\");
    println!("        --webhook-secret <pick a strong secret>");
    Ok(())
}

pub async fn register_webhook(args: RegisterWebhookArgs) -> Result<()> {
    let webhook_url = build_webhook_url(&args.gateway_url);
    let client = Client::new(&args.forgejo_url, &args.token).context("build forgejo client")?;
    let request = CreateWebhookRequest {
        kind: "forgejo".into(),
        config: WebhookConfig {
            url: webhook_url.clone(),
            content_type: "json".into(),
            secret: args.webhook_secret,
        },
        events: vec!["pull_request".into(), "issue_comment".into()],
        active: true,
    };
    let created = client
        .create_webhook(&args.owner, &args.repo, &request)
        .await
        .context("register webhook")?;
    println!(
        "Registered webhook {} on {}/{} → {}",
        created.id, args.owner, args.repo, webhook_url
    );
    Ok(())
}

/// Append `/webhooks/forgejo` to a gateway base URL, normalizing trailing
/// slashes.
pub fn build_webhook_url(gateway_url: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    format!("{trimmed}{WEBHOOK_PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_appends_path() {
        assert_eq!(
            build_webhook_url("https://reviewer.example.com"),
            "https://reviewer.example.com/webhooks/forgejo"
        );
    }

    #[test]
    fn webhook_url_handles_trailing_slash() {
        assert_eq!(
            build_webhook_url("https://reviewer.example.com/"),
            "https://reviewer.example.com/webhooks/forgejo"
        );
    }

    #[test]
    fn webhook_url_handles_subpath() {
        assert_eq!(
            build_webhook_url("https://x.example/auto/"),
            "https://x.example/auto/webhooks/forgejo"
        );
    }
}
