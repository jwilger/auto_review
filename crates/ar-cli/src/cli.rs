use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "auto-review",
    version,
    about = "Operator CLI for the auto_review Forgejo bot."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Gateway lifecycle commands.
    Gateway,

    /// Authentication and token setup commands.
    #[command(subcommand)]
    Auth(AuthCommand),

    /// Forgejo webhook management commands.
    #[command(subcommand)]
    Webhook(WebhookCommand),

    /// Repository configuration commands.
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Review execution commands.
    #[command(subcommand)]
    Review(ReviewCommand),

    /// Benchmark commands.
    #[command(subcommand)]
    Bench(BenchCommand),

    /// Operational diagnostics commands.
    #[command(subcommand)]
    Ops(OpsCommand),

    /// Review-history maintenance commands.
    #[command(subcommand)]
    History(HistoryCommand),

    /// Learning-store maintenance commands.
    #[command(subcommand)]
    Learnings(LearningsCommand),
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Mint a personal access token for the auto_review bot user.
    ///
    /// Uses the bot user's own password (HTTP Basic) to create a PAT with
    /// the scopes the reviewer needs. The PAT is printed once; save it
    /// into AR_FORGEJO_TOKEN.
    Init(InitArgs),
}

#[derive(Subcommand, Debug)]
pub enum WebhookCommand {
    /// Register a webhook on a repository so PR events flow to the
    /// reviewer.
    Register(RegisterWebhookArgs),

    /// List webhooks installed on a repository. Useful for auditing
    /// which webhooks point at the gateway and for finding the id
    /// `unregister-webhook` needs.
    List(ListWebhooksArgs),

    /// Delete a webhook by id. Pair with `list-webhooks` to find
    /// the id, or use `--match-url` to delete the one whose
    /// `config.url` matches a substring (typically the gateway's
    /// public hostname). The `--match-url` form is the safe choice
    /// for scripts that don't know ids ahead of time.
    Unregister(UnregisterWebhookArgs),

    /// Send an HMAC-signed `ping` webhook to a running gateway and
    /// print the response. Smoke-tests the intake path (network
    /// reachability + signature secret + header forwarding through
    /// any reverse-proxy) without firing a real review. Run after
    /// `register-webhook` to confirm the deploy works before waiting
    /// for an actual PR.
    Test(TestWebhookArgs),
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Validate one or more `.auto_review.yaml` configuration files.
    /// Parses each file with the same code path the gateway uses and
    /// surfaces any errors with line numbers. Exits non-zero on
    /// validation failure so this fits cleanly in a pre-commit hook
    /// or CI step.
    Validate(ValidateConfigArgs),
}

#[derive(Subcommand, Debug)]
pub enum ReviewCommand {
    /// Run the full review pipeline once against a specific PR. No
    /// gateway or webhook required — useful for development, demos, and
    /// reproducing reported issues.
    Once(ReviewOnceArgs),
}

#[derive(Subcommand, Debug)]
pub enum BenchCommand {
    /// Replay one or more PR fixtures through the LLM-review step
    /// without touching Forgejo. Reports per-fixture latency, finding
    /// counts, and self-heal attempts; aggregates over the batch.
    /// Useful for picking models, tuning prompts, and tracking
    /// regression in review behaviour over time.
    Run(BenchArgs),
}

#[derive(Subcommand, Debug)]
pub enum OpsCommand {
    /// Probe outbound dependencies (Forgejo API, LLM provider) and
    /// sanity-check the webhook secret. Reports per-check pass /
    /// fail / skip with diagnostic detail. Exit 0 only when every
    /// non-skipped check passes — drop into a deploy script before
    /// `register-webhook`.
    Doctor(DoctorArgs),

    /// Pull `/version`, `/info`, and `/metrics` from a running
    /// gateway and render a one-screen operational summary —
    /// runtime config, review-success rate, key counters,
    /// throttle activity. Complements `doctor` (outbound deps)
    /// and `test-webhook` (intake) with the live-state view.
    Status(StatusArgs),
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommand {
    /// Clear the persistent review-history record for a single
    /// PR so the next webhook triggers a fresh full review
    /// (instead of a `compare` diff against a stale baseline
    /// SHA). Useful after a guideline / model change, or to
    /// recover from a botched review. Operates directly on the
    /// SQLite file the gateway writes to; safe to run while the
    /// gateway is up — SQLite handles concurrent access.
    ResetPr(ResetPrArgs),

    /// Drop review-history rows older than N days. Long-running
    /// deployments accumulate one row per PR ever reviewed;
    /// closed PRs don't need their last_reviewed_sha kept
    /// forever. Wire into a periodic cleanup (systemd timer,
    /// cron) for high-traffic instances. Idempotent.
    Purge(PurgeHistoryArgs),
}

#[derive(Subcommand, Debug)]
pub enum LearningsCommand {
    /// List every learning stored in the persistent
    /// `LearningsStore`. Operators currently can only audit
    /// learnings by inspecting Forgejo PR threads (where
    /// `@<bot> remember` invocations live); this surfaces the
    /// full set in one place. `--json` for piping into a
    /// reviewer tool.
    List(ListLearningsArgs),

    /// Delete a learning by id. Same effect as `@<bot> forget`
    /// but operates directly on the SQLite store, so operators
    /// can script bulk wipes without going through Forgejo.
    /// Use `list-learnings` to find the id.
    Forget(ForgetLearningArgs),
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
pub struct ListLearningsArgs {
    /// Path to the gateway's SQLite learnings database. Reads
    /// `AR_LEARNINGS_DB` by default.
    #[arg(long, env = "AR_LEARNINGS_DB")]
    pub learnings_db: std::path::PathBuf,

    /// Emit the result as one JSON object per line for piping
    /// into `jq`. Otherwise renders a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct ForgetLearningArgs {
    /// Path to the gateway's SQLite learnings database. Reads
    /// `AR_LEARNINGS_DB` by default.
    #[arg(long, env = "AR_LEARNINGS_DB")]
    pub learnings_db: std::path::PathBuf,

    /// Learning id, as printed by `list-learnings`.
    #[arg(long)]
    pub id: u64,
}

#[derive(clap::Args, Debug)]
pub struct PurgeHistoryArgs {
    /// Path to the gateway's SQLite review-history database.
    /// Reads `AR_HISTORY_DB` by default — same env var the
    /// gateway uses, so an operator on the gateway host runs
    /// with just `--older-than-days N`.
    #[arg(long, env = "AR_HISTORY_DB")]
    pub history_db: std::path::PathBuf,

    /// Drop rows whose `updated_at` is older than this many
    /// days. 90 is a reasonable default for repos where
    /// most PRs close within a quarter; raise for repos with
    /// long-lived feature branches.
    #[arg(long)]
    pub older_than_days: u64,

    /// Print what would be dropped without actually deleting.
    /// Useful for first-time runs to gauge the cleanup volume
    /// before committing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::Args, Debug)]
pub struct ResetPrArgs {
    /// Path to the gateway's SQLite review-history database.
    /// Reads `AR_HISTORY_DB` by default — the same env var the
    /// gateway uses, so operators can run this with no args
    /// when both processes share the env.
    #[arg(long, env = "AR_HISTORY_DB")]
    pub history_db: std::path::PathBuf,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Pull-request number whose history record should be
    /// cleared.
    #[arg(long)]
    pub pr: u64,
}

#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Gateway URL the status request goes to. Same value as
    /// `register-webhook --gateway-url` minus the
    /// `/webhooks/forgejo` suffix.
    #[arg(long)]
    pub gateway_url: String,

    /// Emit the parsed result as JSON instead of the human-readable
    /// summary. Pipe into `jq` or another tracker for trend lines.
    #[arg(long)]
    pub json: bool,

    /// Connect timeout in seconds.
    #[arg(long, default_value_t = 10)]
    pub timeout_secs: u64,
}

#[derive(clap::Args, Debug)]
pub struct DoctorArgs {
    /// Forgejo base URL. When set, `doctor` calls
    /// `/api/v1/version` to confirm reachability + token validity
    /// (when `--token` is also set). Skipped otherwise.
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: Option<String>,

    /// Gateway bot PAT. Pair with `--forgejo-url` to validate auth.
    #[arg(long, env = "AR_FORGEJO_TOKEN")]
    pub token: Option<String>,

    /// LLM base URL. When set, `doctor` calls `<base>/v1/models`
    /// to confirm reachability + key validity. Skipped otherwise.
    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: Option<String>,

    /// API key for the LLM provider. Optional for local Ollama.
    #[arg(long, env = "LLM_API_KEY")]
    pub llm_api_key: Option<String>,

    /// Reasoning-tier model name (e.g. `qwen2.5-coder:32b`). When
    /// set alongside `--llm-base-url`, `doctor` confirms the model
    /// appears in `/v1/models`. Catches the common deploy failure
    /// where the env var doesn't match what's actually loaded on
    /// the inference server.
    #[arg(long, env = "LLM_REASONING_MODEL")]
    pub llm_reasoning_model: Option<String>,

    /// Cheap-tier model. Same verification as `--llm-reasoning-model`;
    /// skipped when unset (the cheap tier is optional).
    #[arg(long, env = "LLM_CHEAP_MODEL")]
    pub llm_cheap_model: Option<String>,

    /// Embedding-tier model. Same verification; skipped when unset.
    #[arg(long, env = "LLM_EMBEDDING_MODEL")]
    pub llm_embedding_model: Option<String>,

    /// Webhook secret. When set, `doctor` checks length /
    /// entropy. Skipped otherwise.
    #[arg(long, env = "WEBHOOK_SECRET")]
    pub webhook_secret: Option<String>,

    /// Connect timeout (per check) in seconds.
    #[arg(long, default_value_t = 10)]
    pub timeout_secs: u64,
}

#[derive(clap::Args, Debug)]
pub struct TestWebhookArgs {
    /// Gateway URL the webhook should be POSTed to. The path
    /// `/webhooks/forgejo` is appended (mirroring `register-webhook`).
    #[arg(long)]
    pub gateway_url: String,

    /// Webhook secret. Must match the gateway's `WEBHOOK_SECRET`.
    #[arg(long, env = "WEBHOOK_SECRET")]
    pub webhook_secret: String,

    /// Override the event sent (defaults to `ping`, which the gateway
    /// answers with `200 pong` and never enqueues a review). Use
    /// `pull_request` to round-trip a synthetic PR event through the
    /// dispatcher; the spawned review will fail to reach the (fake)
    /// PR's clone URL but the webhook response still proves intake.
    #[arg(long, default_value = "ping")]
    pub event: String,

    /// Connect timeout in seconds. The gateway acks webhooks
    /// quickly; a slow response means a misconfigured proxy or
    /// network egress problem.
    #[arg(long, default_value_t = 10)]
    pub timeout_secs: u64,
}

#[derive(clap::Args, Debug)]
pub struct ValidateConfigArgs {
    /// One or more `.auto_review.yaml` paths (or directories
    /// containing such files).
    #[arg(required = true)]
    pub paths: Vec<std::path::PathBuf>,

    /// Reject unknown top-level keys. Catches typos like
    /// `enabld:` (missing `e`) that the runtime loader silently
    /// ignores. Recommended for pre-commit hooks; default off
    /// because forward-compatible configs (a config written for
    /// a newer auto_review version) would fail.
    #[arg(long)]
    pub strict: bool,
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

    /// Path to a previous bench run's JSON aggregate (typically
    /// from `auto_review bench --json > baseline.json`). When set,
    /// the current run is compared against this baseline and the
    /// deltas — precision, recall, success rate, mean/p99 latency,
    /// total findings — are printed alongside the aggregate.
    #[arg(long)]
    pub baseline: Option<std::path::PathBuf>,

    /// When set with `--baseline`, exit non-zero on a regression:
    /// precision or recall drops by > 5 percentage points, or p99
    /// latency increases by > 50%. Useful in CI to gate prompt /
    /// model changes.
    #[arg(long, requires = "baseline")]
    pub fail_on_regression: bool,
}

#[derive(clap::Args, Debug)]
pub struct ListWebhooksArgs {
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: String,

    #[arg(long, env = "FORGEJO_TOKEN")]
    pub token: String,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Emit the result as one JSON object per line for piping into
    /// `jq`. Otherwise renders a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct UnregisterWebhookArgs {
    #[arg(long, env = "FORGEJO_BASE_URL")]
    pub forgejo_url: String,

    #[arg(long, env = "FORGEJO_TOKEN")]
    pub token: String,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Webhook id, as printed by `register-webhook` or
    /// `list-webhooks`. Mutually exclusive with `--match-url`.
    #[arg(long, conflicts_with = "match_url")]
    pub id: Option<u64>,

    /// Substring to match against each webhook's `config.url`.
    /// Deletes every webhook whose URL contains the substring.
    /// Use the gateway's public hostname (e.g. `reviewer.example.com`)
    /// to delete only your own bot's hook and leave any others
    /// alone. Mutually exclusive with `--id`.
    #[arg(long)]
    pub match_url: Option<String>,
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
            "auto-review",
            "auth",
            "init",
            "--forgejo-url",
            "https://x.example",
            "--username",
            "bot",
        ])
        .expect("parse");
        match cli.command {
            Command::Auth(AuthCommand::Init(a)) => {
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
            "auto-review",
            "auth",
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
            Command::Auth(AuthCommand::Init(a)) => {
                assert_eq!(a.password.as_deref(), Some("p"));
                assert_eq!(a.scopes, vec!["write:repository", "read:user"]);
            }
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn register_webhook_requires_all_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "register",
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
            Command::Webhook(WebhookCommand::Register(a)) => {
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
            "auto-review",
            "review",
            "once",
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
            "--llm-model",
            "qwen2.5-coder:32b",
            "--llm-api-key",
            "test-key",
        ])
        .expect("parse");
        match cli.command {
            Command::Review(ReviewCommand::Once(a)) => {
                assert_eq!(a.owner, "alice");
                assert_eq!(a.repo, "widgets");
                assert_eq!(a.pr, 42);
                assert_eq!(a.llm_base_url, "http://localhost:11434");
                assert_eq!(a.llm_model, "qwen2.5-coder:32b");
                assert_eq!(a.llm_api_key.as_deref(), Some("test-key"));
            }
            _ => panic!("expected ReviewOnce"),
        }
    }

    #[test]
    fn list_webhooks_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "list",
            "--forgejo-url",
            "https://x.example",
            "--token",
            "tok",
            "--owner",
            "alice",
            "--repo",
            "widgets",
        ])
        .expect("parse");
        match cli.command {
            Command::Webhook(WebhookCommand::List(a)) => {
                assert_eq!(a.owner, "alice");
                assert_eq!(a.repo, "widgets");
                assert!(!a.json);
            }
            _ => panic!("expected ListWebhooks"),
        }
    }

    #[test]
    fn unregister_webhook_accepts_id_or_match_url_but_not_both() {
        // --id is allowed
        let by_id = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "unregister",
            "--forgejo-url",
            "https://x",
            "--token",
            "tok",
            "--owner",
            "alice",
            "--repo",
            "widgets",
            "--id",
            "7",
        ])
        .expect("parse with --id");
        match by_id.command {
            Command::Webhook(WebhookCommand::Unregister(a)) => {
                assert_eq!(a.id, Some(7));
                assert!(a.match_url.is_none());
            }
            _ => panic!("expected UnregisterWebhook"),
        }

        // --match-url is allowed
        let by_match = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "unregister",
            "--forgejo-url",
            "https://x",
            "--token",
            "tok",
            "--owner",
            "alice",
            "--repo",
            "widgets",
            "--match-url",
            "reviewer.example.com",
        ])
        .expect("parse with --match-url");
        match by_match.command {
            Command::Webhook(WebhookCommand::Unregister(a)) => {
                assert!(a.id.is_none());
                assert_eq!(a.match_url.as_deref(), Some("reviewer.example.com"));
            }
            _ => panic!("expected UnregisterWebhook"),
        }

        // both is rejected
        let both = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "unregister",
            "--forgejo-url",
            "https://x",
            "--token",
            "tok",
            "--owner",
            "alice",
            "--repo",
            "widgets",
            "--id",
            "7",
            "--match-url",
            "reviewer",
        ]);
        assert!(
            both.is_err(),
            "--id and --match-url must be mutually exclusive"
        );
    }

    #[test]
    fn list_learnings_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "learnings",
            "list",
            "--learnings-db",
            "/var/lib/auto_review/learnings.db",
        ])
        .expect("parse");
        match cli.command {
            Command::Learnings(LearningsCommand::List(a)) => {
                assert_eq!(
                    a.learnings_db.to_string_lossy(),
                    "/var/lib/auto_review/learnings.db"
                );
                assert!(!a.json);
            }
            _ => panic!("expected ListLearnings"),
        }
    }

    #[test]
    fn forget_learning_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "learnings",
            "forget",
            "--learnings-db",
            "/tmp/x.db",
            "--id",
            "42",
        ])
        .expect("parse");
        match cli.command {
            Command::Learnings(LearningsCommand::Forget(a)) => {
                assert_eq!(a.id, 42);
            }
            _ => panic!("expected ForgetLearning"),
        }
    }

    #[test]
    fn purge_history_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "history",
            "purge",
            "--history-db",
            "/tmp/h.db",
            "--older-than-days",
            "90",
        ])
        .expect("parse");
        match cli.command {
            Command::History(HistoryCommand::Purge(a)) => {
                assert_eq!(a.older_than_days, 90);
                assert!(!a.dry_run);
            }
            _ => panic!("expected PurgeHistory"),
        }
    }

    #[test]
    fn purge_history_dry_run_flag() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "history",
            "purge",
            "--history-db",
            "/tmp/h.db",
            "--older-than-days",
            "30",
            "--dry-run",
        ])
        .expect("parse");
        match cli.command {
            Command::History(HistoryCommand::Purge(a)) => assert!(a.dry_run),
            _ => panic!("expected PurgeHistory"),
        }
    }

    #[test]
    fn reset_pr_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "history",
            "reset-pr",
            "--history-db",
            "/var/lib/auto_review/review_history.db",
            "--owner",
            "alice",
            "--repo",
            "widgets",
            "--pr",
            "42",
        ])
        .expect("parse");
        match cli.command {
            Command::History(HistoryCommand::ResetPr(a)) => {
                assert_eq!(
                    a.history_db.to_string_lossy(),
                    "/var/lib/auto_review/review_history.db"
                );
                assert_eq!(a.owner, "alice");
                assert_eq!(a.repo, "widgets");
                assert_eq!(a.pr, 42);
            }
            _ => panic!("expected ResetPr"),
        }
    }

    #[test]
    fn status_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "ops",
            "status",
            "--gateway-url",
            "https://reviewer.example.com",
        ])
        .expect("parse");
        match cli.command {
            Command::Ops(OpsCommand::Status(a)) => {
                assert_eq!(a.gateway_url, "https://reviewer.example.com");
                assert!(!a.json);
                assert_eq!(a.timeout_secs, 10);
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn doctor_with_no_args_skips_all_checks() {
        let cli = Cli::try_parse_from(["auto-review", "ops", "doctor"]).expect("parse");
        match cli.command {
            Command::Ops(OpsCommand::Doctor(a)) => {
                // Optional fields may be populated from the operator's
                // environment by clap's `env = ...` support. This parse
                // test only verifies the grouped command shape and the
                // argument default that is independent of process env.
                assert_eq!(a.timeout_secs, 10);
            }
            _ => panic!("expected Doctor"),
        }
    }

    #[test]
    fn doctor_with_full_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "ops",
            "doctor",
            "--forgejo-url",
            "https://forge.example",
            "--token",
            "tok",
            "--llm-base-url",
            "http://localhost:11434",
            "--webhook-secret",
            "abcdef0123456789abcdef0123456789",
            "--timeout-secs",
            "30",
        ])
        .expect("parse");
        match cli.command {
            Command::Ops(OpsCommand::Doctor(a)) => {
                assert_eq!(a.forgejo_url.as_deref(), Some("https://forge.example"));
                assert_eq!(a.token.as_deref(), Some("tok"));
                assert_eq!(a.llm_base_url.as_deref(), Some("http://localhost:11434"));
                assert_eq!(a.timeout_secs, 30);
            }
            _ => panic!("expected Doctor"),
        }
    }

    #[test]
    fn test_webhook_required_args() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "test",
            "--gateway-url",
            "http://localhost:8080",
            "--webhook-secret",
            "s",
        ])
        .expect("parse");
        match cli.command {
            Command::Webhook(WebhookCommand::Test(a)) => {
                assert_eq!(a.gateway_url, "http://localhost:8080");
                assert_eq!(a.webhook_secret, "s");
                assert_eq!(a.event, "ping");
                assert_eq!(a.timeout_secs, 10);
            }
            _ => panic!("expected TestWebhook"),
        }
    }

    #[test]
    fn test_webhook_with_pr_event() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "webhook",
            "test",
            "--gateway-url",
            "http://x.example",
            "--webhook-secret",
            "s",
            "--event",
            "pull_request",
            "--timeout-secs",
            "30",
        ])
        .expect("parse");
        match cli.command {
            Command::Webhook(WebhookCommand::Test(a)) => {
                assert_eq!(a.event, "pull_request");
                assert_eq!(a.timeout_secs, 30);
            }
            _ => panic!("expected TestWebhook"),
        }
    }

    #[test]
    fn validate_config_accepts_multiple_paths() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "config",
            "validate",
            "/tmp/a/.auto_review.yaml",
            "/tmp/b",
        ])
        .expect("parse");
        match cli.command {
            Command::Config(ConfigCommand::Validate(a)) => {
                assert_eq!(a.paths.len(), 2);
            }
            _ => panic!("expected ValidateConfig"),
        }
    }

    #[test]
    fn validate_config_requires_at_least_one_path() {
        let res = Cli::try_parse_from(["auto-review", "config", "validate"]);
        assert!(res.is_err());
    }

    #[test]
    fn cargo_public_binary_target_is_auto_review_with_hyphen() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest_path = manifest_dir.join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
        let bin_section = manifest
            .split("[[bin]]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .unwrap_or_else(|| {
                panic!("{} should define a [[bin]] target", manifest_path.display())
            });

        assert!(
            bin_section.lines().any(|line| line.trim() == "name = \"auto-review\""),
            "[[bin]] target should publish the operator binary as `auto-review`; actual [[bin]] section:\n{bin_section}"
        );
    }

    #[test]
    fn public_cli_uses_auto_review_binary_and_grouped_domain_commands() {
        use clap::CommandFactory;

        let cmd = Cli::command();
        let expected_groups = [
            "gateway",
            "auth",
            "webhook",
            "config",
            "review",
            "bench",
            "ops",
            "history",
            "learnings",
        ];
        let actual_groups = cmd
            .get_subcommands()
            .map(|subcommand| subcommand.get_name().to_owned())
            .collect::<Vec<_>>();

        let mut failures = Vec::new();
        if cmd.get_name() != "auto-review" {
            failures.push(format!(
                "binary name should be `auto-review`, got `{}`",
                cmd.get_name()
            ));
        }
        for expected in expected_groups {
            if !actual_groups.iter().any(|actual| actual == expected) {
                failures.push(format!("missing top-level command group `{expected}`"));
            }
        }

        let legacy_review_once = Cli::try_parse_from([
            "auto-review",
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
        ]);
        if legacy_review_once.is_ok() {
            failures.push("legacy flat command `review-once` should not be accepted".to_owned());
        }

        assert!(
            failures.is_empty(),
            "{}\nactual top-level commands: {:?}",
            failures.join("\n"),
            actual_groups
        );
    }

    #[test]
    fn gateway_is_direct_command_without_run_subcommand() {
        let direct_gateway = Cli::try_parse_from(["auto-review", "gateway"]);
        let legacy_gateway_run = Cli::try_parse_from(["auto-review", "gateway", "run"]);

        let mut failures = Vec::new();
        if let Err(error) = direct_gateway {
            failures.push(format!(
                "`auto-review gateway` should be accepted directly: {error}"
            ));
        }
        if legacy_gateway_run.is_ok() {
            failures.push("legacy `auto-review gateway run` should be rejected".to_owned());
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn gateway_command_dispatches_through_shared_gateway_startup_seam() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cli_main_path = manifest.join("src/main.rs");
        let gateway_main_path = manifest.join("../ar-gateway/src/main.rs");
        let gateway_lib_path = manifest.join("../ar-gateway/src/lib.rs");
        let cli_main = std::fs::read_to_string(&cli_main_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", cli_main_path.display()));
        let gateway_lib = std::fs::read_to_string(&gateway_lib_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", gateway_lib_path.display()));

        assert!(
            cli_main.contains("return ar_gateway::run_from_env().await;"),
            "`auto-review gateway` should dispatch through ar_gateway::run_from_env(); actual ar-cli main:\n{cli_main}"
        );
        assert!(
            cli_main.find("return ar_gateway::run_from_env().await;")
                < cli_main.find("tracing_subscriber::fmt()"),
            "`auto-review gateway` should enter ar_gateway::run_from_env() before CLI tracing initialization so gateway defaults are preserved; actual ar-cli main:\n{cli_main}"
        );
        assert!(
            !cli_main.contains("gateway run is not implemented"),
            "`auto-review gateway` should not return the old placeholder error"
        );
        assert!(
            gateway_lib.contains("pub use startup::run_from_env;"),
            "ar-gateway library should expose the shared run_from_env() startup seam"
        );
        assert!(
            !gateway_main_path.exists(),
            "single-binary rollout should not keep a public ar-gateway binary at {}",
            gateway_main_path.display()
        );
    }

    #[test]
    fn flake_publishes_auto_review_as_the_only_operator_binary() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root");
        let flake_path = workspace_root.join("flake.nix");
        let flake = std::fs::read_to_string(&flake_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", flake_path.display()));

        assert!(
            flake.contains("default = self.packages.${system}.ar-cli;"),
            "nix default package should publish the unified auto-review binary"
        );
        assert!(
            flake.contains("Cmd = [ \"${ar-cli}/bin/auto-review\" \"gateway\" ];"),
            "gateway container should start through auto-review gateway"
        );
        assert!(
            !flake.contains("cargoExtraArgs = \"-p ar-gateway --bin ar-gateway\""),
            "flake should not publish an ar-gateway binary package"
        );
    }

    /// Cross-file contract test: every subcommand `Cli` exposes
    /// must appear in `crates/ar-cli/README.md`. Adding a new
    /// subcommand without documenting it in the per-crate README
    /// is the kind of drift that's invisible until an operator
    /// goes looking for the feature and fails to find it.
    #[test]
    fn readme_documents_every_subcommand() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let readme_path = manifest.join("README.md");
        let body = std::fs::read_to_string(&readme_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", readme_path.display()));
        let mut missing = Vec::new();
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            // The README documents subcommands as `\`name\`` — match
            // that literal so a stray substring elsewhere doesn't
            // false-positive.
            let needle = format!("`{name}`");
            if !body.contains(&needle) {
                missing.push(name.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "subcommands missing from ar-cli/README.md: {missing:?}"
        );
    }

    #[test]
    fn missing_required_arg_is_an_error() {
        let res = Cli::try_parse_from([
            "auto-review",
            "auth",
            "init",
            "--forgejo-url",
            "https://x",
            // username missing
        ]);
        assert!(res.is_err());
    }
}
