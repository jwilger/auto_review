//! Clap definitions for the `auto-review` CLI: subcommands, flags, env
//! bindings and their value parsers.

use ar_index::{LearningsFilter, RepoScope};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Gateway lifecycle commands.
    Gateway(GatewayArgs),

    /// AWS Bedrock AgentCore runtime commands.
    #[command(subcommand)]
    Agentcore(AgentcoreCommand),

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

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct GatewayArgs {
    /// Run the gateway directly with only application-level controls.
    #[arg(long)]
    pub bare: bool,
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum AgentcoreCommand {
    /// Serve the AgentCore-compatible runtime HTTP API.
    Serve(AgentcoreServeArgs),
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct AgentcoreServeArgs {
    /// Address for the AgentCore runtime HTTP server.
    #[arg(long, default_value = "0.0.0.0:9000", env = "AGENTCORE_BIND")]
    pub bind: String,

    /// Forgejo base URL for semantic-review invocations.
    #[arg(long = "forgejo-url", env = "FORGEJO_BASE_URL")]
    pub forgejo_url: Option<String>,

    /// Forgejo bot token for semantic-review invocations.
    #[arg(long = "token", env = "AR_FORGEJO_TOKEN")]
    pub token: Option<String>,

    /// GitHub REST API base URL for semantic-review invocations.
    #[arg(
        long = "github-api-url",
        default_value = "https://api.github.com",
        env = "GITHUB_API_URL"
    )]
    pub github_api_url: String,

    /// GitHub App id for semantic-review invocations.
    #[arg(long = "github-app-id", env = "GITHUB_APP_ID")]
    pub github_app_id: Option<u64>,

    /// GitHub App private key PEM for semantic-review invocations.
    #[arg(long = "github-app-private-key", env = "GITHUB_APP_PRIVATE_KEY")]
    pub github_app_private_key: Option<String>,

    /// OpenAI-compatible reasoning provider base URL.
    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: Option<String>,

    /// OpenAI-compatible API key for the reasoning provider.
    #[arg(long, env = "LLM_API_KEY")]
    pub llm_api_key: Option<String>,

    /// Reasoning model name.
    #[arg(long, default_value = "qwen2.5-coder:32b", env = "LLM_REASONING_MODEL")]
    pub llm_model: String,

    /// Omit `temperature` from every chat request. Set to true for `OpenAI`
    /// reasoning models (gpt-5.x, gpt-6.x, o-series), which reject a
    /// non-default temperature; leave unset for Ollama and gpt-4.x.
    #[arg(
        long,
        env = "LLM_OMIT_TEMPERATURE",
        value_name = "BOOL",
        default_value = "false",
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = true,
        action = clap::ArgAction::Set,
        value_parser = parse_llm_omit_temperature
    )]
    pub llm_omit_temperature: bool,

    /// DynamoDB table for durable AgentCore invocation idempotency.
    #[arg(
        long = "idempotency-dynamodb-table",
        env = "AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE"
    )]
    pub idempotency_dynamodb_table: Option<String>,

    /// Seconds before DynamoDB idempotency records expire.
    #[arg(
        long = "idempotency-ttl-secs",
        default_value_t = 86_400,
        env = "AGENTCORE_IDEMPOTENCY_TTL_SECS"
    )]
    pub idempotency_ttl_secs: u64,

    /// DynamoDB table for durable AgentCore review history.
    #[arg(
        long = "history-dynamodb-table",
        env = "AGENTCORE_HISTORY_DYNAMODB_TABLE"
    )]
    pub history_dynamodb_table: Option<String>,

    /// DynamoDB table for durable AgentCore learnings.
    #[arg(
        long = "learnings-dynamodb-table",
        env = "AGENTCORE_LEARNINGS_DYNAMODB_TABLE"
    )]
    pub learnings_dynamodb_table: Option<String>,
}

#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Mint a personal access token for the auto_review bot user.
    ///
    /// Uses the bot user's own password (HTTP Basic) to create a PAT with
    /// the scopes the reviewer needs. The PAT is printed once; save it
    /// into AR_FORGEJO_TOKEN.
    Init(InitArgs),
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum WebhookCommand {
    /// Register a webhook on a repository so PR events flow to the
    /// reviewer.
    Register(RegisterWebhookArgs),

    /// List webhooks installed on a repository. Useful for auditing
    /// which webhooks point at the gateway and for finding the id
    /// `webhook unregister` needs.
    List(ListWebhooksArgs),

    /// Delete a webhook by id. Pair with `webhook list` to find
    /// the id, or use `--match-url` to delete the one whose
    /// `config.url` matches a substring (typically the gateway's
    /// public hostname). The `--match-url` form is the safe choice
    /// for scripts that don't know ids ahead of time.
    Unregister(UnregisterWebhookArgs),

    /// Send an HMAC-signed `ping` webhook to a running gateway and
    /// print the response. Smoke-tests the intake path (network
    /// reachability + signature secret + header forwarding through
    /// any reverse-proxy) without firing a real review. Run after
    /// `webhook register` to confirm the deploy works before waiting
    /// for an actual PR.
    Test(TestWebhookArgs),
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Validate one or more `.auto_review.yaml` configuration files.
    /// Parses each file with the same code path the gateway uses and
    /// surfaces any errors with line numbers. Exits non-zero on
    /// validation failure so this fits cleanly in a pre-commit hook
    /// or CI step.
    Validate(ValidateConfigArgs),
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum ReviewCommand {
    /// Run a one-shot reasoning-path review against a specific PR. No
    /// gateway or webhook required — useful for development, demos, and
    /// reproducing reported issues. This does not wire every gateway runtime
    /// store or optional tier.
    Once(ReviewOnceArgs),
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum BenchCommand {
    /// Replay one or more PR fixtures through the LLM-review step
    /// without touching Forgejo. Reports per-fixture latency, finding
    /// counts, and self-heal attempts; aggregates over the batch.
    /// Useful for picking models, tuning prompts, and tracking
    /// regression in review behaviour over time.
    Run(BenchArgs),
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum OpsCommand {
    /// Probe outbound dependencies (Forgejo API, LLM provider) and
    /// sanity-check the webhook secret. Reports per-check pass /
    /// fail / skip with diagnostic detail. Exit 0 only when every
    /// non-skipped check passes — drop into a deploy script before
    /// `webhook register`.
    Doctor(DoctorArgs),

    /// Pull `/version`, `/info`, and `/metrics` from a running
    /// gateway and render a one-screen operational summary —
    /// runtime config, review-success rate, key counters,
    /// throttle activity. Complements `doctor` (outbound deps)
    /// and `webhook test` (intake) with the live-state view.
    Status(StatusArgs),
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(Subcommand, Debug)]
pub enum LearningsCommand {
    /// List every learning stored in the persistent
    /// `LearningsStore`. Operators currently can only audit
    /// learnings by inspecting Forgejo PR threads (where
    /// `@<bot> remember` invocations live); this surfaces the
    /// full set in one place. `--json` for piping into a
    /// reviewer tool.
    List(ListLearningsArgs),

    /// Delete a learning by id, in any repository scope or
    /// unscoped. Unlike `@<bot> forget`, which only removes the
    /// PR repository's learnings, this operates directly on the
    /// store, so operators can script bulk wipes without going
    /// through a forge.
    /// Use `learnings list` to find the id.
    Forget(ForgetLearningArgs),
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

    /// Omit `temperature` from every chat request. Set to true for `OpenAI`
    /// reasoning models (gpt-5.x, gpt-6.x, o-series), which reject a
    /// non-default temperature; leave unset for Ollama and gpt-4.x.
    #[arg(
        long,
        env = "LLM_OMIT_TEMPERATURE",
        value_name = "BOOL",
        default_value = "false",
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = true,
        action = clap::ArgAction::Set,
        value_parser = parse_llm_omit_temperature
    )]
    pub llm_omit_temperature: bool,

    /// Print the rendered base LLM prompt and exit. Skips clone, RAG context,
    /// repo config loading, LLM calls, and posting. Useful for debugging prompt
    /// shape without burning tokens or touching the PR.
    #[arg(long)]
    pub dry_run: bool,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct ListLearningsArgs {
    /// Path to the gateway's SQLite learnings database. Reads
    /// `AR_LEARNINGS_DB` by default.
    #[arg(long, env = "AR_LEARNINGS_DB")]
    pub learnings_db: PathBuf,

    /// Emit the result as one JSON object per line for piping
    /// into `jq`. Otherwise renders a human-readable table.
    #[arg(long)]
    pub json: bool,

    /// Only list learnings scoped to this repository, given as
    /// `owner/repo`. Matching is case-insensitive.
    #[arg(long, value_name = "OWNER/REPO", value_parser = parse_repo_scope)]
    pub repo: Option<RepoScope>,

    /// Only list legacy learnings that have no repository. These are
    /// excluded from review retrieval.
    #[arg(long, conflicts_with = "repo")]
    pub unscoped: bool,
}

impl ListLearningsArgs {
    /// The store filter the flags select. With neither flag every
    /// learning is listed.
    pub fn filter(&self) -> LearningsFilter {
        match (self.repo.as_ref(), self.unscoped) {
            (Some(scope), _) => LearningsFilter::Repo(scope.clone()),
            (None, true) => LearningsFilter::Unscoped,
            (None, false) => LearningsFilter::All,
        }
    }
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct ForgetLearningArgs {
    /// Path to the gateway's SQLite learnings database. Reads
    /// `AR_LEARNINGS_DB` by default.
    #[arg(long, env = "AR_LEARNINGS_DB")]
    pub learnings_db: PathBuf,

    /// Learning id, as printed by `learnings list`.
    #[arg(long)]
    pub id: u64,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct PurgeHistoryArgs {
    /// Path to the gateway's SQLite review-history database.
    /// Reads `AR_HISTORY_DB` by default — same env var the
    /// gateway uses, so an operator on the gateway host runs
    /// with just `--older-than-days N`.
    #[arg(long, env = "AR_HISTORY_DB")]
    pub history_db: PathBuf,

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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct ResetPrArgs {
    /// Path to the gateway's SQLite review-history database.
    /// Reads `AR_HISTORY_DB` by default — the same env var the
    /// gateway uses, so operators can run this with no args
    /// when both processes share the env.
    #[arg(long, env = "AR_HISTORY_DB")]
    pub history_db: PathBuf,

    #[arg(long)]
    pub owner: String,

    #[arg(long)]
    pub repo: String,

    /// Pull-request number whose history record should be
    /// cleared.
    #[arg(long)]
    pub pr: u64,
}

#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Gateway URL the status request goes to. Same value as
    /// `webhook register --gateway-url` minus the
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct TestWebhookArgs {
    /// Gateway URL the webhook should be POSTed to. The path
    /// `/webhooks/forgejo` is appended (mirroring `webhook register`).
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

#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct ValidateConfigArgs {
    /// One or more `.auto_review.yaml` paths (or directories
    /// containing such files).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Reject unknown top-level keys. Catches typos like
    /// `enabld:` (missing `e`) that the runtime loader silently
    /// ignores. Recommended for pre-commit hooks; default off
    /// because forward-compatible configs (a config written for
    /// a newer auto_review version) would fail.
    #[arg(long)]
    pub strict: bool,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    /// One or more fixture file paths or directories. Each fixture is
    /// a JSON file with the shape documented in `docs/BENCHMARKS.md`.
    /// When a directory is given, every `*.json` file in it is loaded.
    #[arg(required = true)]
    pub fixtures: Vec<PathBuf>,

    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: String,

    #[arg(long, env = "LLM_API_KEY")]
    pub llm_api_key: Option<String>,

    #[arg(long, env = "LLM_REASONING_MODEL", default_value = "qwen2.5-coder:32b")]
    pub llm_model: String,

    /// Omit `temperature` from every chat request. Set to true for `OpenAI`
    /// reasoning models (gpt-5.x, gpt-6.x, o-series), which reject a
    /// non-default temperature; leave unset for Ollama and gpt-4.x.
    #[arg(
        long,
        env = "LLM_OMIT_TEMPERATURE",
        value_name = "BOOL",
        default_value = "false",
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = true,
        action = clap::ArgAction::Set,
        value_parser = parse_llm_omit_temperature
    )]
    pub llm_omit_temperature: bool,

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
    /// from `auto-review bench run --json > baseline.json`). When set,
    /// the current run is compared against this baseline and the
    /// deltas — precision, recall, success rate, mean/p99 latency,
    /// total findings — are printed alongside the aggregate.
    #[arg(long)]
    pub baseline: Option<PathBuf>,

    /// When set with `--baseline`, exit non-zero on a regression:
    /// precision or recall drops by > 5 percentage points, or p99
    /// latency increases by > 50%. Useful in CI to gate prompt /
    /// model changes.
    #[arg(long, requires = "baseline")]
    pub fail_on_regression: bool,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

    /// Webhook id, as printed by `webhook register` or
    /// `webhook list`. Mutually exclusive with `--match-url`.
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

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "clap lists subcommands and flags in --help in declaration order; sorting them alphabetically would reorder the CLI's help output"
)]
#[expect(
    clippy::doc_markdown,
    reason = "clap prints these doc comments verbatim as --help text, so Markdown backticks or <> around URLs would show up in the CLI's help output"
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "clap derive turns doc comments on this item and its fields into --help text; adding docs here would change the CLI's help output"
    )
)]
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

/// Scopes `auth init` requests when `--scopes` is not given.
fn default_scopes() -> Vec<String> {
    vec![
        "write:repository".into(),
        "write:issue".into(),
        "read:user".into(),
    ]
}

/// Parse `--llm-omit-temperature` / `LLM_OMIT_TEMPERATURE`, using
/// the same vocabulary as the gateway: true/false, yes/no, on/off, 1/0
/// (case-insensitive). A blank value means unset, i.e. false. Anything
/// else is rejected so a typo cannot silently keep the old behaviour.
fn parse_llm_omit_temperature(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        _ => Err("LLM_OMIT_TEMPERATURE takes true/false, yes/no, on/off, or 1/0".to_owned()),
    }
}

/// Parse `--repo` as an `owner/repo` learning scope.
fn parse_repo_scope(value: &str) -> Result<RepoScope, String> {
    RepoScope::parse(value.trim()).ok_or_else(|| format!("expected owner/repo, got {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[expect(
        clippy::assert_is_empty,
        reason = "test code: the assertion stays as written; it checks emptiness without pinning the exact contents"
    )]
    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Auth(AuthCommand::Init(parsed)) => {
                assert_eq!(parsed.forgejo_url, "https://x.example");
                assert_eq!(parsed.username, "bot");
                assert!(parsed.password.is_none());
                assert_eq!(parsed.token_name, "auto_review");
                assert!(!parsed.scopes.is_empty());
            }
            _ => panic!("expected Init"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Auth(AuthCommand::Init(parsed)) => {
                assert_eq!(parsed.password.as_deref(), Some("p"));
                assert_eq!(parsed.scopes, vec!["write:repository", "read:user"]);
            }
            _ => panic!("expected Init"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Webhook(WebhookCommand::Register(parsed)) => {
                assert_eq!(parsed.owner, "o");
                assert_eq!(parsed.repo, "r");
                assert_eq!(parsed.gateway_url, "https://g.example");
            }
            _ => panic!("expected RegisterWebhook"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test helper: it returns the parse Result for the caller to inspect, and a parse into the wrong subcommand is a test failure, not an error to return"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    fn review_once_with(extra: &[&str]) -> Result<ReviewOnceArgs, clap::Error> {
        let mut argv = vec![
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
        ];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv)?.command {
            Command::Review(ReviewCommand::Once(parsed)) => Ok(parsed),
            other => panic!("expected ReviewOnce, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn llm_omit_temperature_flag_accepts_bool_vocabulary() {
        assert!(
            review_once_with(&["--llm-omit-temperature=YES"])
                .expect("parse")
                .llm_omit_temperature
        );
        assert!(
            review_once_with(&["--llm-omit-temperature"])
                .expect("bare flag parses")
                .llm_omit_temperature
        );
        assert!(
            !review_once_with(&["--llm-omit-temperature=off"])
                .expect("parse")
                .llm_omit_temperature
        );
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn llm_omit_temperature_flag_rejects_garbage_naming_the_flag() {
        let err = review_once_with(&["--llm-omit-temperature=maybe"])
            .expect_err("garbage must not parse");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().contains("--llm-omit-temperature"), "{err}");
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn bare_llm_omit_temperature_does_not_swallow_bench_fixture_path() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "bench",
            "run",
            "--llm-base-url",
            "http://localhost:11434",
            "--llm-omit-temperature",
            "fixtures/x.json",
        ])
        .expect("bare flag followed by a fixture path parses");
        match cli.command {
            Command::Bench(BenchCommand::Run(parsed)) => {
                assert!(parsed.llm_omit_temperature);
                assert_eq!(parsed.fixtures, vec![PathBuf::from("fixtures/x.json")]);
            }
            other => panic!("expected BenchRun, got {other:?}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[test]
    fn llm_omit_temperature_is_on_every_provider_building_command() {
        use clap::CommandFactory as _;

        let cmd = Cli::command();
        for path in [["agentcore", "serve"], ["review", "once"], ["bench", "run"]] {
            let sub = cmd
                .find_subcommand(path[0])
                .and_then(|group| group.find_subcommand(path[1]))
                .unwrap_or_else(|| panic!("missing subcommand {path:?}"));
            let arg = sub
                .get_arguments()
                .find(|arg| arg.get_id() == "llm_omit_temperature")
                .unwrap_or_else(|| panic!("{path:?} lacks --llm-omit-temperature"));
            assert_eq!(
                arg.get_env().and_then(|value| value.to_str()),
                Some("LLM_OMIT_TEMPERATURE"),
                "{path:?}"
            );
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Review(ReviewCommand::Once(parsed)) => {
                assert_eq!(parsed.owner, "alice");
                assert_eq!(parsed.repo, "widgets");
                assert_eq!(parsed.pr, 42);
                assert_eq!(parsed.llm_base_url, "http://localhost:11434");
                assert_eq!(parsed.llm_model, "qwen2.5-coder:32b");
                assert_eq!(parsed.llm_api_key.as_deref(), Some("test-key"));
            }
            _ => panic!("expected ReviewOnce"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Webhook(WebhookCommand::List(parsed)) => {
                assert_eq!(parsed.owner, "alice");
                assert_eq!(parsed.repo, "widgets");
                assert!(!parsed.json);
            }
            _ => panic!("expected ListWebhooks"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Webhook(WebhookCommand::Unregister(parsed)) => {
                assert_eq!(parsed.id, Some(7));
                assert!(parsed.match_url.is_none());
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
            Command::Webhook(WebhookCommand::Unregister(parsed)) => {
                assert!(parsed.id.is_none());
                assert_eq!(parsed.match_url.as_deref(), Some("reviewer.example.com"));
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

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Learnings(LearningsCommand::List(parsed)) => {
                assert_eq!(
                    parsed.learnings_db.to_string_lossy(),
                    "/var/lib/auto_review/learnings.db"
                );
                assert!(!parsed.json);
            }
            _ => panic!("expected ListLearnings"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test helper: it returns the parse Result for the caller to inspect, and a parse into the wrong subcommand is a test failure, not an error to return"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    fn parse_list_learnings(extra: &[&str]) -> Result<ListLearningsArgs, clap::Error> {
        let mut argv = vec![
            "auto-review",
            "learnings",
            "list",
            "--learnings-db",
            "/tmp/l.db",
        ];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv)?.command {
            Command::Learnings(LearningsCommand::List(parsed)) => Ok(parsed),
            other => panic!("expected ListLearnings, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn list_learnings_repo_flag_filters_to_the_lowercased_repository() {
        let args = parse_list_learnings(&["--repo", "Acme/Widget"]).expect("parse");
        assert_eq!(
            args.filter(),
            ar_index::LearningsFilter::Repo(ar_index::RepoScope::new("acme", "widget"))
        );
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn list_learnings_unscoped_flag_filters_to_unscoped_learnings() {
        let args = parse_list_learnings(&["--unscoped"]).expect("parse");
        assert_eq!(args.filter(), ar_index::LearningsFilter::Unscoped);
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn list_learnings_lists_every_learning_by_default() {
        let args = parse_list_learnings(&[]).expect("parse");
        assert_eq!(args.filter(), ar_index::LearningsFilter::All);
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn list_learnings_rejects_repo_and_unscoped_together() {
        let err = parse_list_learnings(&["--repo", "acme/widget", "--unscoped"])
            .expect_err("--repo and --unscoped must conflict");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[test]
    fn list_learnings_rejects_repo_without_owner() {
        let err = parse_list_learnings(&["--repo", "widget"]).expect_err("needs owner/repo");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Learnings(LearningsCommand::Forget(parsed)) => {
                assert_eq!(parsed.id, 42);
            }
            _ => panic!("expected ForgetLearning"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::History(HistoryCommand::Purge(parsed)) => {
                assert_eq!(parsed.older_than_days, 90);
                assert!(!parsed.dry_run);
            }
            _ => panic!("expected PurgeHistory"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::History(HistoryCommand::Purge(parsed)) => assert!(parsed.dry_run),
            _ => panic!("expected PurgeHistory"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::History(HistoryCommand::ResetPr(parsed)) => {
                assert_eq!(
                    parsed.history_db.to_string_lossy(),
                    "/var/lib/auto_review/review_history.db"
                );
                assert_eq!(parsed.owner, "alice");
                assert_eq!(parsed.repo, "widgets");
                assert_eq!(parsed.pr, 42);
            }
            _ => panic!("expected ResetPr"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Ops(OpsCommand::Status(parsed)) => {
                assert_eq!(parsed.gateway_url, "https://reviewer.example.com");
                assert!(!parsed.json);
                assert_eq!(parsed.timeout_secs, 10);
            }
            _ => panic!("expected Status"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn doctor_with_no_args_skips_all_checks() {
        let cli = Cli::try_parse_from(["auto-review", "ops", "doctor"]).expect("parse");
        match cli.command {
            Command::Ops(OpsCommand::Doctor(parsed)) => {
                // Optional fields may be populated from the operator's
                // environment by clap's `env = ...` support. This parse
                // test only verifies the grouped command shape and the
                // argument default that is independent of process env.
                assert_eq!(parsed.timeout_secs, 10);
            }
            _ => panic!("expected Doctor"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Ops(OpsCommand::Doctor(parsed)) => {
                assert_eq!(parsed.forgejo_url.as_deref(), Some("https://forge.example"));
                assert_eq!(parsed.token.as_deref(), Some("tok"));
                assert_eq!(
                    parsed.llm_base_url.as_deref(),
                    Some("http://localhost:11434")
                );
                assert_eq!(parsed.timeout_secs, 30);
            }
            _ => panic!("expected Doctor"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::redundant_test_prefix,
        reason = "test_webhook names the `webhook test` subcommand under test, not a test-function prefix"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Webhook(WebhookCommand::Test(parsed)) => {
                assert_eq!(parsed.gateway_url, "http://localhost:8080");
                assert_eq!(parsed.webhook_secret, "s");
                assert_eq!(parsed.event, "ping");
                assert_eq!(parsed.timeout_secs, 10);
            }
            _ => panic!("expected TestWebhook"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::redundant_test_prefix,
        reason = "test_webhook names the `webhook test` subcommand under test, not a test-function prefix"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Webhook(WebhookCommand::Test(parsed)) => {
                assert_eq!(parsed.event, "pull_request");
                assert_eq!(parsed.timeout_secs, 30);
            }
            _ => panic!("expected TestWebhook"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
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
            Command::Config(ConfigCommand::Validate(parsed)) => {
                assert_eq!(parsed.paths.len(), 2);
            }
            _ => panic!("expected ValidateConfig"),
        }
    }

    #[expect(
        clippy::assertions_on_result_states,
        reason = "test code: the assertion only checks that the call fails and stays as written; the error value is not part of the contract under test"
    )]
    #[test]
    fn validate_config_requires_at_least_one_path() {
        let res = Cli::try_parse_from(["auto-review", "config", "validate"]);
        assert!(res.is_err());
    }

    #[test]
    fn public_cli_uses_auto_review_binary_and_grouped_domain_commands() {
        use clap::CommandFactory as _;

        let cmd = Cli::command();
        let expected_groups = [
            "gateway",
            "agentcore",
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

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_bind_address() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--bind",
            "0.0.0.0:9000",
        ])
        .expect("agentcore serve parses");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(args.bind, "0.0.0.0:9000");
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_forgejo_review_runtime_inputs() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--forgejo-url",
            "https://git.example",
            "--token",
            "forgejo-token",
            "--llm-base-url",
            "https://llm.example/v1",
            "--llm-model",
            "review-model",
        ])
        .expect("agentcore serve parses runtime inputs");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(args.forgejo_url.as_deref(), Some("https://git.example"));
                assert_eq!(args.token.as_deref(), Some("forgejo-token"));
                assert_eq!(args.llm_base_url.as_deref(), Some("https://llm.example/v1"));
                assert_eq!(args.llm_model, "review-model");
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_github_app_review_runtime_inputs() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--github-api-url",
            "https://api.github.example",
            "--github-app-id",
            "12345",
            "--github-app-private-key",
            "private-key-pem",
            "--llm-base-url",
            "https://llm.example/v1",
        ])
        .expect("agentcore serve parses GitHub runtime inputs");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(args.github_api_url, "https://api.github.example");
                assert_eq!(args.github_app_id, Some(12345));
                assert_eq!(
                    args.github_app_private_key.as_deref(),
                    Some("private-key-pem")
                );
                assert_eq!(args.llm_base_url.as_deref(), Some("https://llm.example/v1"));
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_dynamodb_idempotency_inputs() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--idempotency-dynamodb-table",
            "auto-review-agentcore-idempotency",
            "--idempotency-ttl-secs",
            "900",
        ])
        .expect("agentcore serve parses idempotency inputs");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(
                    args.idempotency_dynamodb_table.as_deref(),
                    Some("auto-review-agentcore-idempotency")
                );
                assert_eq!(args.idempotency_ttl_secs, 900);
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_dynamodb_history_inputs() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--history-dynamodb-table",
            "auto-review-agentcore-history",
        ])
        .expect("agentcore serve parses history inputs");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(
                    args.history_dynamodb_table.as_deref(),
                    Some("auto-review-agentcore-history")
                );
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: the test checks for one variant, and every other variant, including future ones, is a failure"
    )]
    #[test]
    fn agentcore_serve_parses_dynamodb_learnings_inputs() {
        let cli = Cli::try_parse_from([
            "auto-review",
            "agentcore",
            "serve",
            "--learnings-dynamodb-table",
            "auto-review-agentcore-learnings",
        ])
        .expect("agentcore serve parses learnings inputs");

        match cli.command {
            Command::Agentcore(AgentcoreCommand::Serve(args)) => {
                assert_eq!(
                    args.learnings_dynamodb_table.as_deref(),
                    Some("auto-review-agentcore-learnings")
                );
            }
            other => panic!("expected agentcore serve command, got {other:?}"),
        }
    }

    #[test]
    fn gateway_accepts_bare_opt_out_without_legacy_run_subcommand() {
        let direct_gateway = Cli::try_parse_from(["auto-review", "gateway"]);
        let bare_gateway = Cli::try_parse_from(["auto-review", "gateway", "--bare"]);
        let legacy_gateway_run = Cli::try_parse_from(["auto-review", "gateway", "run"]);

        let mut failures = Vec::new();
        match direct_gateway.map(|cli| cli.command) {
            Ok(Command::Gateway(args)) => {
                if args.bare {
                    failures.push(
                        "`auto-review gateway` should default to non-bare startup".to_owned(),
                    );
                }
            }
            Ok(other) => failures.push(format!(
                "`auto-review gateway` should parse as Command::Gateway, got {other:?}"
            )),
            Err(error) => failures.push(format!(
                "`auto-review gateway` should be accepted directly: {error}"
            )),
        }
        match bare_gateway.map(|cli| cli.command) {
            Ok(Command::Gateway(args)) => {
                if !args.bare {
                    failures.push("`auto-review gateway --bare` should set the explicit bare opt-out".to_owned());
                }
            }
            Ok(other) => failures.push(format!(
                "`auto-review gateway --bare` should parse as Command::Gateway, got {other:?}"
            )),
            Err(error) => failures.push(format!(
                "`auto-review gateway --bare` should be accepted as an explicit bare opt-out: {error}"
            )),
        }
        if legacy_gateway_run.is_ok() {
            failures.push("legacy `auto-review gateway run` should be rejected".to_owned());
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[expect(
        clippy::assertions_on_result_states,
        reason = "test code: the assertion only checks that the call fails and stays as written; the error value is not part of the contract under test"
    )]
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
