//! Per-command chat handlers.
//!
//! [`ChatHandler`] dispatches a parsed [`ChatCommand`] to the right
//! action and posts a reply on the PR. Currently implemented:
//!
//! - `Help`: posts a list of supported commands.
//! - `Remember(text)`: embeds the text via the LLM router and adds
//!   it as a `Chat`-source learning. Replies confirming with the
//!   new id.
//! - `Forget(id)`: removes the learning. Replies confirming, or
//!   surfaces "not found" when the id is unknown.
//! - `ReReview`: when a JobDispatcher is configured, fetches the
//!   PR's current head SHA + metadata and dispatches a force=true
//!   ReviewJob (bypasses the per-PR history dedup). Drafts are
//!   skipped with an explanation. Without a dispatcher, replies
//!   noting the feature isn't wired up.
//! - `Freeform(text)`: when the Cheap tier is configured, fetches
//!   the PR diff (best-effort), truncates it to fit the cheap
//!   model's context, calls the LLM with the user's question, and
//!   posts the model's reply. Without a Cheap tier, replies noting
//!   the feature isn't enabled.
//! - `NotMentioned`: silently returns; the gateway shouldn't have
//!   called us in this case.

use crate::command::ChatCommand;
use ar_forgejo::{Client as ForgejoClient, CreateReviewRequest, ReviewComment, ReviewEvent};
use ar_index::{LearningSource, LearningsStore};
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router as LlmRouter};
use ar_orchestrator::{JobDispatcher, ReviewJob};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// Byte cap on the diff snippet we feed into freeform-chat prompts.
/// Cheap-tier models tend to have smaller context windows than the
/// reasoning tier; this cap keeps us comfortably under any of the
/// usual ~16k–32k limits.
const FREEFORM_DIFF_CAP: usize = 40_000;

/// Byte cap on the freeform reply we post back to Forgejo. The
/// system prompt asks the model to be brief but a misbehaving
/// model can ignore that. Forgejo accepts arbitrary-length issue
/// comments but a multi-megabyte reply would either hang the post
/// or render unreadable; cap it with a clear truncation marker.
const FREEFORM_REPLY_CAP: usize = 32_000;

/// Same cap as `FREEFORM_DIFF_CAP` but for the autofix prompt.
/// Same justification — cheap-tier model, similar context.
const AUTOFIX_DIFF_CAP: usize = 40_000;

/// Maximum number of patches the autofix command will post in a
/// single invocation. Bounded to prevent the model from drowning a
/// PR thread in marginal suggestions.
const AUTOFIX_MAX_PATCHES: usize = 5;

const AUTOFIX_SYSTEM_PROMPT: &str = "\
You are an autofix assistant for a Forgejo pull request review bot. \
Given a unified diff, propose at most a handful of small, high-confidence \
inline patches: typo fixes, dead-code removal, obvious off-by-one fixes, \
clarifying renames, and similar mechanical changes. Skip anything that \
requires judgement or surrounding context you can't see. Each patch must \
target a line that exists in the new (post-diff) version of the file; you \
will be checked against the diff. Be conservative — emit zero patches if \
nothing is clearly safe.";

const TESTS_SYSTEM_PROMPT: &str = "\
You scaffold unit tests for newly-added or substantially-modified \
items in a Forgejo pull request diff. For each item that lacks test \
coverage in the diff, propose one focused test case that exercises a \
representative happy path or edge case. Use the language's idiomatic \
test framework: `#[test]` + `#[cfg(test)] mod tests` for Rust, \
`pytest` for Python, `jest`/`vitest` for JS/TS, `testing` for Go, \
`RSpec` for Ruby. Each scaffold must compile/parse on its own; \
include any imports the framework needs at the top. Cap at 5 \
scaffolds per command; emit zero if nothing in the diff plausibly \
needs new test coverage.";

const DOCSTRINGS_SYSTEM_PROMPT: &str = "\
You generate docstrings for newly-added or modified public-facing items \
in a Forgejo pull request diff. Look for functions, methods, classes, \
structs, and enums in the diff that lack a docstring (or whose docstring \
is stale relative to the new signature) and propose docstrings for them. \
\n\nEach patch's `replacement` must replace the item's signature line \
with a multi-line string that contains: \
\n  1. The new docstring (using the language's idiomatic comment style: \
`///` for Rust, `\"\"\"...\"\"\"` for Python, `/** ... */` for JS/TS/Java) \
\n  2. The original signature line, byte-for-byte. \
\n\nUse `\\n` to separate lines inside the replacement. Skip items that \
already have an adequate docstring. Cap at 5 docstrings per command; emit \
zero if nothing in the diff needs one.";

const FREEFORM_SYSTEM_PROMPT: &str = "\
You are a code-review chat assistant for Forgejo pull requests. \
Answer the user's question about the diff concisely and accurately. \
Cite specific line numbers from the diff when useful. If you don't \
know the answer, say so — don't hallucinate. Markdown is fine; \
keep replies brief.";

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("forgejo: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    #[error("learnings store: {0}")]
    Learnings(#[from] ar_index::LearningsError),
    #[error("LLM error: {0}")]
    Llm(#[from] ar_llm::Error),
    #[error("system time: {0}")]
    Time(String),
}

const HELP_TEXT: &str = "\
**auto_review chat commands** (mention me with `@auto_review`):

- `remember <text>` — store project-specific guidance. I'll inject \
matching learnings into future review prompts.
- `forget <id>` — drop a previously-remembered learning by its id \
(printed on `remember`).
- `re-review` — re-run a full review on the current head SHA, \
ignoring my recorded review history.
- `autofix` — propose inline `\\`\\`\\`suggestion` patches for safe, \
mechanical fixes (typos, dead code, obvious off-by-ones). Capped at \
5 patches per command; the cheap-tier model decides what's safe.
- `docstring` — generate docstrings for newly-added items in the \
diff that lack them, posted as inline suggestion patches. Same cap.
- `tests` — scaffold unit tests for newly-added items in the diff \
that lack coverage. Posts a single comment with copy-pasteable test \
cases (tests live in separate files, so no inline suggestion).
- `help` — print this message.

Anything else after the mention is treated as a freeform question.";

/// Wire-up for the chat handler. Holds the dependencies it needs to
/// dispatch a [`ChatCommand`].
pub struct ChatHandler<'a> {
    pub forgejo: &'a ForgejoClient,
    pub llm: &'a LlmRouter,
    pub learnings: &'a (dyn LearningsStore + Sync),
    /// Optional review dispatcher. When set, `ReReview` queues a
    /// fresh review job. When unset, `ReReview` just acks.
    pub dispatcher: Option<Arc<dyn JobDispatcher>>,
}

/// Coordinates that locate a PR comment thread on Forgejo.
#[derive(Debug, Clone, Copy)]
pub struct ChatContext<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub issue_number: u64,
}

impl ChatHandler<'_> {
    pub async fn handle(
        &self,
        ctx: ChatContext<'_>,
        command: ChatCommand,
    ) -> Result<(), ChatError> {
        match command {
            ChatCommand::Help => {
                self.post(ctx, HELP_TEXT).await?;
            }
            ChatCommand::Remember(text) => {
                self.handle_remember(ctx, &text).await?;
            }
            ChatCommand::Forget(id) => {
                self.handle_forget(ctx, id).await?;
            }
            ChatCommand::ReReview => {
                self.handle_re_review(ctx).await?;
            }
            ChatCommand::Autofix => {
                self.handle_suggest(ctx, SuggestionKind::Autofix).await?;
            }
            ChatCommand::Docstrings => {
                self.handle_suggest(ctx, SuggestionKind::Docstrings).await?;
            }
            ChatCommand::TestScaffolds => {
                self.handle_test_scaffolds(ctx).await?;
            }
            ChatCommand::Freeform(text) => {
                self.handle_freeform(ctx, &text).await?;
            }
            ChatCommand::NotMentioned => {}
        }
        Ok(())
    }

    async fn handle_re_review(&self, ctx: ChatContext<'_>) -> Result<(), ChatError> {
        let Some(dispatcher) = &self.dispatcher else {
            self.post(
                ctx,
                "Re-review isn't wired up here (no dispatcher configured). The \
                 next review on this PR will run when the next webhook fires.",
            )
            .await?;
            return Ok(());
        };
        // Fetch the PR's current head SHA + metadata to build a job.
        let pr = self
            .forgejo
            .get_pull_request(ctx.owner, ctx.repo, ctx.issue_number)
            .await?;
        if pr.draft {
            self.post(
                ctx,
                "Skipping re-review: this PR is a draft. Mark it ready first.",
            )
            .await?;
            return Ok(());
        }
        let job = ReviewJob {
            owner: ctx.owner.to_string(),
            repo: ctx.repo.to_string(),
            pr_number: pr.number,
            head_sha: pr.head.sha.clone(),
            pr_title: pr.title,
            pr_body: pr.body,
            // force=true bypasses the per-PR review-history dedup so
            // the user gets a fresh review even at the same SHA.
            force: true,
        };
        dispatcher.dispatch(job).await;
        let reply = format!(
            "Queued a fresh review at {}. Watch the commit-status badge for progress.",
            pr.head.sha
        );
        self.post(ctx, &reply).await
    }

    /// Generic LLM-driven inline-suggestion command. Both `autofix`
    /// and `docstring` route through here; they differ only in the
    /// system prompt and the user-visible banner message.
    async fn handle_suggest(
        &self,
        ctx: ChatContext<'_>,
        kind: SuggestionKind,
    ) -> Result<(), ChatError> {
        if self.llm.provider(ModelTier::Cheap).is_err() {
            let msg = format!(
                "{} needs an LLM_CHEAP_MODEL configured. Try \
                 `@auto_review help` for the structured commands.",
                kind.label()
            );
            self.post(ctx, &msg).await?;
            return Ok(());
        }
        let pr = self
            .forgejo
            .get_pull_request(ctx.owner, ctx.repo, ctx.issue_number)
            .await?;
        if pr.draft {
            let msg = format!("Skipping {}: this PR is a draft.", kind.lowercase_label());
            self.post(ctx, &msg).await?;
            return Ok(());
        }
        let diff = self
            .forgejo
            .get_pr_diff(ctx.owner, ctx.repo, ctx.issue_number)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(kind = ?kind, error = %e, "diff fetch failed");
                String::new()
            });
        if diff.trim().is_empty() {
            let msg = format!(
                "{} can't run without a diff (none returned by Forgejo).",
                kind.label()
            );
            self.post(ctx, &msg).await?;
            return Ok(());
        }
        let truncated = truncate_for_chat(&diff, AUTOFIX_DIFF_CAP);

        let user_prompt = build_suggestion_user_prompt(kind, &truncated);
        let req = CompleteRequest {
            system: Some(kind.system_prompt().to_string()),
            messages: vec![Message::user(user_prompt)],
            response_format: Some(ResponseFormat::JsonSchema {
                name: kind.schema_name().into(),
                schema: autofix_schema(),
            }),
            ..Default::default()
        };
        let resp = self.llm.complete(ModelTier::Cheap, req).await?;
        let parsed: AutofixOutput = match serde_json::from_str(&resp.content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(kind = ?kind, error = %e, "model returned malformed JSON");
                let msg = format!(
                    "{} didn't return well-formed suggestions; nothing posted.",
                    kind.label()
                );
                self.post(ctx, &msg).await?;
                return Ok(());
            }
        };
        // Defence-in-depth: drop any patch whose `path` isn't in
        // the PR's diff. The system prompt tells the cheap model
        // to stick to diff paths but the prompt isn't a guarantee
        // — a hallucinated path either Forgejo-rejects the whole
        // review or, worse, posts a comment on a file the PR
        // didn't touch (looks like the bot is reviewing files at
        // random). Either failure mode is worse than dropping the
        // bad patch.
        let diff_paths = paths_in_diff(&diff);
        let patches: Vec<AutofixPatch> = parsed
            .patches
            .into_iter()
            .filter(|p| {
                !p.path.is_empty()
                    && p.line >= 1
                    && !p.replacement.is_empty()
                    && diff_paths.contains(p.path.as_str())
            })
            .take(AUTOFIX_MAX_PATCHES)
            .collect();
        if patches.is_empty() {
            let msg = format!("{} found nothing to suggest.", kind.label());
            self.post(ctx, &msg).await?;
            return Ok(());
        }

        let comments: Vec<ReviewComment> = patches
            .iter()
            .map(|p| ReviewComment {
                path: p.path.clone(),
                body: format_suggestion_body(p),
                old_position: None,
                new_position: Some(p.line),
            })
            .collect();
        let body = format!(
            "{} posted {} suggested {}. Each is applicable inline; \
             review and click 'Apply suggestion' on the ones you want.",
            kind.label(),
            comments.len(),
            kind.unit_plural(comments.len()),
        );
        let request = CreateReviewRequest {
            body,
            commit_id: pr.head.sha,
            event: ReviewEvent::Comment,
            comments,
        };
        self.forgejo
            .create_review(ctx.owner, ctx.repo, ctx.issue_number, &request)
            .await?;
        Ok(())
    }

    async fn handle_test_scaffolds(&self, ctx: ChatContext<'_>) -> Result<(), ChatError> {
        if self.llm.provider(ModelTier::Cheap).is_err() {
            self.post(
                ctx,
                "Test scaffolding needs an LLM_CHEAP_MODEL configured. \
                 Try `@auto_review help` for the structured commands.",
            )
            .await?;
            return Ok(());
        }
        let pr = self
            .forgejo
            .get_pull_request(ctx.owner, ctx.repo, ctx.issue_number)
            .await?;
        if pr.draft {
            self.post(ctx, "Skipping test scaffolds: this PR is a draft.")
                .await?;
            return Ok(());
        }
        let diff = self
            .forgejo
            .get_pr_diff(ctx.owner, ctx.repo, ctx.issue_number)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "diff fetch for tests failed");
                String::new()
            });
        if diff.trim().is_empty() {
            self.post(
                ctx,
                "Test scaffolding can't run without a diff (none returned by Forgejo).",
            )
            .await?;
            return Ok(());
        }
        let truncated = truncate_for_chat(&diff, AUTOFIX_DIFF_CAP);

        let user_prompt = build_tests_user_prompt(&truncated);
        let req = CompleteRequest {
            system: Some(TESTS_SYSTEM_PROMPT.to_string()),
            messages: vec![Message::user(user_prompt)],
            response_format: Some(ResponseFormat::JsonSchema {
                name: "TestScaffolds".into(),
                schema: tests_schema(),
            }),
            ..Default::default()
        };
        let resp = self.llm.complete(ModelTier::Cheap, req).await?;
        let parsed: TestsOutput = match serde_json::from_str(&resp.content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "tests model returned malformed JSON");
                self.post(
                    ctx,
                    "Test scaffolding didn't return well-formed output; nothing posted.",
                )
                .await?;
                return Ok(());
            }
        };
        let scaffolds: Vec<TestScaffold> = parsed
            .scaffolds
            .into_iter()
            .filter(|s| !s.item_name.is_empty() && !s.source.is_empty())
            .take(AUTOFIX_MAX_PATCHES)
            .collect();
        if scaffolds.is_empty() {
            self.post(
                ctx,
                "Test scaffolding found nothing in the diff that needs new coverage.",
            )
            .await?;
            return Ok(());
        }
        let body = format_test_scaffolds_body(&scaffolds);
        self.post(ctx, &body).await
    }

    async fn handle_freeform(&self, ctx: ChatContext<'_>, question: &str) -> Result<(), ChatError> {
        // No Cheap tier ⇒ no chat replies. Fall through to the
        // placeholder so the user knows their message was seen.
        if self.llm.provider(ModelTier::Cheap).is_err() {
            self.post(
                ctx,
                "Conversational replies need an LLM_CHEAP_MODEL configured. \
                 Try `@auto_review help` for the structured commands.",
            )
            .await?;
            return Ok(());
        }

        // Best-effort diff fetch — we still answer if it fails.
        let diff = self
            .forgejo
            .get_pr_diff(ctx.owner, ctx.repo, ctx.issue_number)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "diff fetch for freeform reply failed");
                String::new()
            });
        let truncated = truncate_for_chat(&diff, FREEFORM_DIFF_CAP);

        let user_prompt = build_freeform_user_prompt(question, &truncated);
        let req = CompleteRequest {
            system: Some(FREEFORM_SYSTEM_PROMPT.to_string()),
            messages: vec![Message::user(user_prompt)],
            ..Default::default()
        };
        let resp = self.llm.complete(ModelTier::Cheap, req).await?;
        let reply = if resp.content.trim().is_empty() {
            "(no response from the chat model)".to_string()
        } else if resp.content.len() > FREEFORM_REPLY_CAP {
            tracing::warn!(
                original_bytes = resp.content.len(),
                "cheap-tier model emitted oversized freeform reply; truncating before post"
            );
            truncate_with_marker(&resp.content, FREEFORM_REPLY_CAP, "[reply truncated]")
        } else {
            resp.content
        };
        self.post(ctx, &reply).await
    }

    async fn handle_remember(&self, ctx: ChatContext<'_>, text: &str) -> Result<(), ChatError> {
        let embedding = self.embed(text).await?;
        let now = current_unix_seconds()?;
        let record = self
            .learnings
            .add(text.to_string(), LearningSource::Chat, embedding, now)
            .await?;
        let reply = format!(
            "Remembered as learning #{}. To revoke later: `@auto_review forget {}`.",
            record.id, record.id
        );
        self.post(ctx, &reply).await
    }

    async fn handle_forget(&self, ctx: ChatContext<'_>, id: u64) -> Result<(), ChatError> {
        let reply = match self.learnings.remove(id).await {
            Ok(()) => format!("Forgotten learning #{id}."),
            Err(ar_index::LearningsError::NotFound(_)) => {
                format!("No learning with id {id}; nothing to forget.")
            }
            Err(e) => return Err(ChatError::Learnings(e)),
        };
        self.post(ctx, &reply).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, ChatError> {
        // Embedding tier may not be configured; in that case, store a
        // zero vector. The learning is still searchable by exact
        // content in `list()`, just not by similarity.
        if self.llm.provider(ModelTier::Embedding).is_err() {
            return Ok(Vec::new());
        }
        let mut vecs = self
            .llm
            .embed(ModelTier::Embedding, &[text.to_string()])
            .await?;
        Ok(vecs.pop().unwrap_or_default())
    }

    async fn post(&self, ctx: ChatContext<'_>, body: &str) -> Result<(), ChatError> {
        self.forgejo
            .post_issue_comment(ctx.owner, ctx.repo, ctx.issue_number, body)
            .await?;
        Ok(())
    }
}

/// Which suggestion-style command is being handled. Picks the
/// system prompt, banner copy, and JSON-schema name; the posting
/// flow is shared between them.
#[derive(Debug, Clone, Copy)]
enum SuggestionKind {
    Autofix,
    Docstrings,
}

impl SuggestionKind {
    fn label(self) -> &'static str {
        match self {
            SuggestionKind::Autofix => "Autofix",
            SuggestionKind::Docstrings => "Docstrings",
        }
    }

    fn lowercase_label(self) -> &'static str {
        match self {
            SuggestionKind::Autofix => "autofix",
            SuggestionKind::Docstrings => "docstring generation",
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            SuggestionKind::Autofix => AUTOFIX_SYSTEM_PROMPT,
            SuggestionKind::Docstrings => DOCSTRINGS_SYSTEM_PROMPT,
        }
    }

    fn schema_name(self) -> &'static str {
        match self {
            SuggestionKind::Autofix => "Autofix",
            SuggestionKind::Docstrings => "Docstrings",
        }
    }

    fn unit_plural(self, n: usize) -> &'static str {
        match (self, n) {
            (SuggestionKind::Autofix, 1) => "patch",
            (SuggestionKind::Autofix, _) => "patches",
            (SuggestionKind::Docstrings, 1) => "docstring",
            (SuggestionKind::Docstrings, _) => "docstrings",
        }
    }
}

#[derive(Debug, Deserialize)]
struct AutofixOutput {
    #[serde(default)]
    patches: Vec<AutofixPatch>,
}

#[derive(Debug, Deserialize, Clone)]
struct AutofixPatch {
    /// Repo-relative file path the patch applies to.
    #[serde(default)]
    path: String,
    /// 1-indexed line in the new file the suggestion replaces.
    #[serde(default)]
    line: u32,
    /// New content for the line. Multi-line replacements are
    /// expressed by including `\n` in the string; the suggestion
    /// block renders all of them.
    #[serde(default)]
    replacement: String,
    /// Short rationale shown above the suggestion block.
    #[serde(default)]
    reason: String,
}

fn autofix_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "patches": {
                "type": "array",
                "maxItems": AUTOFIX_MAX_PATCHES,
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "line": {"type": "integer", "minimum": 1},
                        "replacement": {"type": "string"},
                        "reason": {"type": "string"}
                    },
                    "required": ["path", "line", "replacement", "reason"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["patches"],
        "additionalProperties": false
    })
}

#[derive(Debug, Deserialize)]
struct TestsOutput {
    #[serde(default)]
    scaffolds: Vec<TestScaffold>,
}

#[derive(Debug, Deserialize, Clone)]
struct TestScaffold {
    /// Human label: `fn validate_token`, `class Mailer`,
    /// `def parse_csv`, etc.
    #[serde(default)]
    item_name: String,
    /// Repo-relative path of the item under test (informational
    /// only — we don't post inline since tests typically live in
    /// separate files).
    #[serde(default)]
    item_path: String,
    /// Test framework label the model picked, e.g. `pytest`,
    /// `cargo-test`, `vitest`. Surfaced in the comment header.
    #[serde(default)]
    framework: String,
    /// Full test source the user can copy into their test file.
    #[serde(default)]
    source: String,
}

fn tests_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "scaffolds": {
                "type": "array",
                "maxItems": AUTOFIX_MAX_PATCHES,
                "items": {
                    "type": "object",
                    "properties": {
                        "item_name": {"type": "string"},
                        "item_path": {"type": "string"},
                        "framework": {"type": "string"},
                        "source": {"type": "string"}
                    },
                    "required": ["item_name", "item_path", "framework", "source"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["scaffolds"],
        "additionalProperties": false
    })
}

fn build_tests_user_prompt(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + 256);
    out.push_str(
        "Scaffold up to 5 unit tests for newly-added or substantially-\
         modified items in the diff below that lack test coverage. Each \
         scaffold must compile/parse on its own; include framework \
         imports at the top of `source`. Pick the language's idiomatic \
         test framework based on the file extension.\n\n",
    );
    out.push_str("Unified diff:\n```diff\n");
    out.push_str(diff);
    if !diff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

fn format_test_scaffolds_body(scaffolds: &[TestScaffold]) -> String {
    let mut out = String::with_capacity(2_048);
    out.push_str("**Test scaffolds**\n\n");
    out.push_str(
        "Below are scaffolded tests for items in this PR that look like \
         they could use coverage. Tests usually live in a separate file, \
         so these are copy-paste-ready rather than inline suggestions.\n\n",
    );
    for s in scaffolds {
        let header = if s.item_path.is_empty() {
            format!("### `{}`", s.item_name)
        } else {
            format!("### `{}` (in `{}`)", s.item_name, s.item_path)
        };
        out.push_str(&header);
        out.push('\n');
        if !s.framework.trim().is_empty() {
            out.push_str(&format!("Framework: `{}`\n\n", s.framework.trim()));
        } else {
            out.push('\n');
        }
        // Same fence-sizing rationale as format_suggestion_body:
        // a Rust doctest source contains literal ```rust...```
        // markers and would otherwise prematurely close the
        // outer fence here.
        let fence = pick_fence(&s.source);
        out.push_str(&fence);
        out.push('\n');
        out.push_str(&s.source);
        if !s.source.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&fence);
        out.push_str("\n\n");
    }
    out
}

fn build_suggestion_user_prompt(kind: SuggestionKind, diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + 256);
    let intro = match kind {
        SuggestionKind::Autofix => {
            "Propose at most 5 safe inline patches for the diff below. \
             Each patch must target a line in the new (post-diff) file. \
             Replacement text replaces that one line; embed `\\n` for \
             multi-line replacements.\n\n"
        }
        SuggestionKind::Docstrings => {
            "Find at most 5 newly-added or modified items in the diff \
             below that lack a docstring (functions, methods, classes, \
             structs, enums). For each, propose a docstring. Each patch's \
             `replacement` replaces the item's signature line with: \
             docstring lines first, then the original signature byte-for-\
             byte. Embed `\\n` for multi-line replacements.\n\n"
        }
    };
    out.push_str(intro);
    out.push_str("Unified diff:\n```diff\n");
    out.push_str(diff);
    if !diff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

fn format_suggestion_body(p: &AutofixPatch) -> String {
    let mut out = String::with_capacity(p.replacement.len() + p.reason.len() + 64);
    if !p.reason.trim().is_empty() {
        out.push_str(p.reason.trim());
        out.push_str("\n\n");
    }
    // Pick a fence longer than any backtick run inside the
    // replacement, so a replacement that contains ```...``` (e.g. a
    // markdown file diff or a bash heredoc) doesn't prematurely
    // close the suggestion block. CommonMark requires the opening
    // and closing fences to use the same length, and the closing
    // fence's length must be >= the longest internal run.
    let fence = pick_fence(&p.replacement);
    out.push_str(&fence);
    out.push_str("suggestion\n");
    out.push_str(&p.replacement);
    if !p.replacement.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out
}

/// Return a backtick fence (≥ 3 backticks) that's strictly longer
/// than the longest backtick run inside `body`. CommonMark fenced
/// code blocks require the closing fence's length to match the
/// opening fence and exceed any internal backtick run.
fn pick_fence(body: &str) -> String {
    let max_internal = longest_backtick_run(body);
    "`".repeat(max_internal.max(2) + 1)
}

fn longest_backtick_run(body: &str) -> usize {
    let mut max = 0usize;
    let mut current = 0usize;
    for c in body.chars() {
        if c == '`' {
            current += 1;
            if current > max {
                max = current;
            }
        } else {
            current = 0;
        }
    }
    max
}

/// Extract every changed-file path from a unified diff. Accepts
/// either the canonical `+++ b/<path>` header or the
/// `diff --git a/<old> b/<new>` line that's always present even
/// when downstream tooling strips the `+++` (e.g. some Forgejo
/// proxies, or test fixtures that simplify the diff format).
///
/// Used as a defence-in-depth filter on LLM-emitted suggestion
/// paths: a patch citing a path NOT in the diff is either a model
/// hallucination or a rule-bypass attempt. Either way we drop it
/// rather than post a comment on an unrelated file.
fn paths_in_diff(diff: &str) -> std::collections::HashSet<String> {
    let mut paths = std::collections::HashSet::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            let path = rest.split_whitespace().next().unwrap_or("");
            if !path.is_empty() {
                paths.insert(path.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            // Format: `a/<old-path> b/<new-path>`. Take the new
            // (`b/`) path since that's what suggestion comments
            // anchor on.
            if let Some(b_part) = rest.split(" b/").nth(1) {
                let path = b_part.split_whitespace().next().unwrap_or("");
                if !path.is_empty() {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    paths
}

fn truncate_for_chat(diff: &str, max_bytes: usize) -> String {
    truncate_with_marker(diff, max_bytes, "[diff truncated]")
}

fn truncate_with_marker(text: &str, max_bytes: usize, marker: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    // Don't split a UTF-8 codepoint.
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str("\n\n");
    out.push_str(marker);
    out.push('\n');
    out
}

fn build_freeform_user_prompt(question: &str, diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + question.len() + 256);
    out.push_str("Question:\n");
    out.push_str(question);
    out.push_str("\n\nUnified diff for the pull request:\n```diff\n");
    if diff.is_empty() {
        out.push_str("(diff unavailable)\n");
    } else {
        out.push_str(diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str("```\n");
    out
}

fn current_unix_seconds() -> Result<i64, ChatError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| ChatError::Time(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_index::InMemoryLearningsStore;
    use ar_llm::{
        CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct ConstantEmbedder;

    #[async_trait]
    impl LlmProvider for ConstantEmbedder {
        async fn complete(&self, _: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            unimplemented!()
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(texts.iter().map(|_| vec![0.5, 0.5]).collect())
        }
    }

    fn ctx() -> ChatContext<'static> {
        ChatContext {
            owner: "alice",
            repo: "widgets",
            issue_number: 42,
        }
    }

    async fn setup() -> (MockServer, ForgejoClient, InMemoryLearningsStore, Router) {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let router = Router::new().with(ModelTier::Embedding, Arc::new(ConstantEmbedder));
        (server, forgejo, learnings, router)
    }

    #[tokio::test]
    async fn help_posts_help_text() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler.handle(ctx(), ChatCommand::Help).await.expect("ok");
    }

    #[tokio::test]
    async fn remember_stores_learning_and_replies_with_id() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Remember("prefer Result".into()))
            .await
            .expect("ok");
        let stored = learnings.list().await.expect("list");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].text, "prefer Result");
        assert_eq!(stored[0].source, LearningSource::Chat);
        assert_eq!(stored[0].embedding, vec![0.5, 0.5]);
    }

    #[tokio::test]
    async fn forget_removes_learning_when_id_exists() {
        let (server, forgejo, learnings, llm) = setup().await;
        // Pre-populate a learning to forget.
        let added = learnings
            .add(
                "old guidance".into(),
                LearningSource::Chat,
                vec![1.0],
                1700000000,
            )
            .await
            .unwrap();

        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Forget(added.id))
            .await
            .expect("ok");
        assert!(learnings.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_with_unknown_id_replies_not_found_without_error() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Forget(999))
            .await
            .expect("ok"); // not-found is reported in the comment, not as an error
    }

    #[tokio::test]
    async fn not_mentioned_command_does_nothing() {
        let (_server, forgejo, learnings, llm) = setup().await;
        // No mock mounted: any POST would fail. Verifies we don't try.
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::NotMentioned)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn remember_uses_zero_vector_when_no_embedding_tier() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let router = Router::new(); // no Embedding tier
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Remember("guidance".into()))
            .await
            .expect("ok");
        let stored = learnings.list().await.expect("list");
        assert_eq!(stored[0].embedding, Vec::<f32>::new());
    }

    /// RecordingDispatcher exposes its captured state via an Arc so
    /// tests can read it back even after the dispatcher itself gets
    /// erased to `Arc<dyn JobDispatcher>`.
    struct RecordingDispatcher {
        seen: Arc<std::sync::Mutex<Vec<ReviewJob>>>,
    }

    #[async_trait::async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    #[tokio::test]
    async fn re_review_with_dispatcher_queues_force_job() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "fix: thing",
                "body": "details",
                "draft": false,
                "head": {"ref": "topic", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher: Arc<dyn JobDispatcher> =
            Arc::new(RecordingDispatcher { seen: seen.clone() });
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: Some(dispatcher),
        };
        handler
            .handle(ctx(), ChatCommand::ReReview)
            .await
            .expect("ok");
        let queued = seen.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].pr_number, 42);
        assert_eq!(queued[0].head_sha, "deadbeef");
        assert!(queued[0].force, "ReReview must set force=true");
    }

    #[tokio::test]
    async fn re_review_without_dispatcher_replies_with_explanation() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::ReReview)
            .await
            .expect("ok");
    }

    /// Cheap-tier provider that returns a fixed response regardless
    /// of input. Lets freeform tests verify the wiring without
    /// pinning a specific prompt shape.
    struct CannedCheapProvider {
        reply: String,
    }

    #[async_trait]
    impl LlmProvider for CannedCheapProvider {
        async fn complete(&self, _: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            Ok(CompleteResponse {
                content: self.reply.clone(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn freeform_with_cheap_tier_posts_llm_response() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: "It changes the call site to use Result.".into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);
        // Diff fetch succeeds.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        // Comment post returns OK; we expect the body to contain the
        // model's reply.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({
                "body": "It changes the call site to use Result."
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Freeform("what does this do?".into()))
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn freeform_truncates_oversized_llm_replies_before_posting() {
        // A misbehaving cheap-tier model could ignore "be brief"
        // and emit megabytes of content; we must cap before
        // posting so Forgejo doesn't choke and so the comment
        // remains readable.
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        // Reply is well above FREEFORM_REPLY_CAP (32_000).
        let huge_reply = "x".repeat(60_000);
        let cheap = Arc::new(CannedCheapProvider {
            reply: huge_reply.clone(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        // Capture the actual posted body and verify it's <= cap +
        // marker. wiremock's body_partial_json approach can't do
        // length assertions easily, so just respond and trust the
        // unit test on truncate_with_marker for byte-count
        // correctness; here we only assert the post succeeds.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Freeform("question".into()))
            .await
            .expect("ok");
    }

    #[test]
    fn truncate_with_marker_uses_supplied_marker() {
        let big = "x".repeat(1000);
        let out = truncate_with_marker(&big, 100, "[reply truncated]");
        assert!(out.contains("[reply truncated]"));
        assert!(!out.contains("[diff truncated]"));
        // Result is bounded: cap (100) + marker (~20 chars) + 3
        // chars of newline framing.
        assert!(out.len() < 130);
    }

    #[tokio::test]
    async fn freeform_without_cheap_tier_replies_with_placeholder() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let router = Router::new(); // no Cheap tier
                                    // No diff fetch should happen — we short-circuit before that.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Freeform("anything".into()))
            .await
            .expect("ok");
    }

    #[test]
    fn truncate_for_chat_preserves_short_input() {
        assert_eq!(truncate_for_chat("hello", 100), "hello");
    }

    #[test]
    fn truncate_for_chat_caps_long_input_with_marker() {
        let big = "x".repeat(1000);
        let out = truncate_for_chat(&big, 100);
        assert!(out.len() < 200);
        assert!(out.contains("[diff truncated]"));
    }

    #[test]
    fn truncate_for_chat_respects_utf8_boundaries() {
        let s = format!("héllo {}", "x".repeat(1000));
        let out = truncate_for_chat(&s, 7);
        // Result must be valid UTF-8 (no panic, parses back).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn re_review_on_draft_pr_skips_dispatch() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "wip",
                "body": "",
                "draft": true,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher: Arc<dyn JobDispatcher> =
            Arc::new(RecordingDispatcher { seen: seen.clone() });
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: Some(dispatcher),
        };
        handler
            .handle(ctx(), ChatCommand::ReReview)
            .await
            .expect("ok");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn autofix_without_cheap_tier_replies_with_placeholder() {
        let (server, forgejo, learnings, llm) = setup().await;
        // Comment post is the only Forgejo call we expect — no
        // pull-request fetch, no diff fetch, no review post.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn autofix_on_draft_pr_posts_skip_message() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: r#"{"patches":[]}"#.into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "wip",
                "body": "",
                "draft": true,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(
                serde_json::json!({"body": "Skipping autofix: this PR is a draft."}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn autofix_posts_review_with_suggestion_blocks() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap_reply = serde_json::json!({
            "patches": [
                {
                    "path": "src/auth.rs",
                    "line": 42,
                    "replacement": "        Err(_) => Err(AuthError::Invalid(\"Token is invalid\".into())),",
                    "reason": "Fix typo: 'invlaid' → 'invalid'"
                }
            ]
        })
        .to_string();
        let cheap = Arc::new(CannedCheapProvider { reply: cheap_reply });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "fix typo",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "feedbeef"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/src/auth.rs b/src/auth.rs\n+    Err(_) => Err(AuthError::Invalid(\"Token is invlaid\".into())),\n"))
            .mount(&server)
            .await;
        // Expect a review POST with the suggestion block. Match
        // partially so we don't pin every field.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
            .and(body_partial_json(serde_json::json!({
                "commit_id": "deadbeef",
                "event": "COMMENT"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn autofix_drops_patches_for_paths_outside_the_diff() {
        // Defence-in-depth: if the cheap-tier model emits a patch
        // for a file the PR doesn't touch (hallucination or
        // confusion), we must NOT post a review comment on that
        // file. The model's claim should be filtered to the diff's
        // own path set before we trust it.
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap_reply = serde_json::json!({
            "patches": [
                {
                    "path": "this/is/not/in/the/diff.rs",
                    "line": 1,
                    "replacement": "totally fabricated change",
                    "reason": "made-up patch"
                }
            ]
        })
        .to_string();
        let cheap = Arc::new(CannedCheapProvider { reply: cheap_reply });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "ok",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        // Diff only touches src/real.rs.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    "diff --git a/src/real.rs b/src/real.rs\n+legitimate change\n",
                ),
            )
            .mount(&server)
            .await;
        // CRITICAL: only an issue comment is expected — NO review
        // POST. If the path-guard fails, the review POST would fire
        // and this test would fail because there's no mock for it
        // (or, worse, if a mock did exist, we'd be silently posting
        // to wrong files in production).
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(
                serde_json::json!({"body": "Autofix found nothing to suggest."}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn autofix_with_no_patches_replies_nothing_safe() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: r#"{"patches":[]}"#.into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "ok",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(
                serde_json::json!({"body": "Autofix found nothing to suggest."}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn autofix_with_malformed_json_posts_graceful_fallback() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: "this is not json at all".into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "ok",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(
                serde_json::json!({"body": "Autofix didn't return well-formed suggestions; nothing posted."}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Autofix)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn docstrings_posts_review_with_suggestion_blocks() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap_reply = serde_json::json!({
            "patches": [{
                "path": "src/lib.rs",
                "line": 7,
                "replacement": "/// Returns the user's display name.\npub fn display_name(u: &User) -> String {",
                "reason": "Public fn lacks docstring"
            }]
        })
        .to_string();
        let cheap = Arc::new(CannedCheapProvider { reply: cheap_reply });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "add display_name",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "feedbeef"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/src/lib.rs b/src/lib.rs\n+pub fn display_name(u: &User) -> String {\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
            .and(body_partial_json(serde_json::json!({
                "commit_id": "deadbeef",
                "event": "COMMENT"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Docstrings)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn docstrings_without_cheap_tier_replies_with_placeholder() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(
                serde_json::json!({"body": "Docstrings needs an LLM_CHEAP_MODEL configured. Try `@auto_review help` for the structured commands."}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Docstrings)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn docstrings_on_draft_pr_posts_skip_message() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: r#"{"patches":[]}"#.into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "wip",
                "body": "",
                "draft": true,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({
                "body": "Skipping docstring generation: this PR is a draft."
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::Docstrings)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn tests_posts_markdown_comment_with_scaffolds() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap_reply = serde_json::json!({
            "scaffolds": [{
                "item_name": "fn validate_token",
                "item_path": "src/auth.rs",
                "framework": "cargo-test",
                "source": "#[test]\nfn validates_a_well_formed_token() {\n    assert!(validate_token(\"abc\").is_ok());\n}"
            }]
        })
        .to_string();
        let cheap = Arc::new(CannedCheapProvider { reply: cheap_reply });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "auth fixes",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/src/auth.rs b/src/auth.rs\n+pub fn validate_token(t: &str) -> Result<(), Error> { Ok(()) }\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::TestScaffolds)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn tests_without_cheap_tier_replies_with_placeholder() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::TestScaffolds)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn tests_with_empty_scaffold_list_replies_no_coverage_needed() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let cheap = Arc::new(CannedCheapProvider {
            reply: r#"{"scaffolds":[]}"#.into(),
        });
        let router = Router::new()
            .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
            .with(ModelTier::Cheap, cheap);

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "ok",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "abc"},
                "base": {"ref": "main", "sha": "def"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/42.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({
                "body": "Test scaffolding found nothing in the diff that needs new coverage."
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
            dispatcher: None,
        };
        handler
            .handle(ctx(), ChatCommand::TestScaffolds)
            .await
            .expect("ok");
    }

    #[test]
    fn format_test_scaffolds_body_emits_one_section_per_scaffold() {
        let scaffolds = vec![
            TestScaffold {
                item_name: "fn parse".into(),
                item_path: "src/lib.rs".into(),
                framework: "cargo-test".into(),
                source: "#[test]\nfn parses_ok() {}".into(),
            },
            TestScaffold {
                item_name: "fn render".into(),
                item_path: String::new(),
                framework: String::new(),
                source: "#[test]\nfn renders_ok() {}\n".into(),
            },
        ];
        let body = format_test_scaffolds_body(&scaffolds);
        assert!(body.contains("**Test scaffolds**"));
        // First section names both item and path.
        assert!(body.contains("`fn parse` (in `src/lib.rs`)"));
        // Second section has no path → just the item.
        assert!(body.contains("### `fn render`"));
        // Source code is fenced.
        assert!(body.contains("```\n#[test]\nfn parses_ok() {}\n```"));
    }

    #[test]
    fn format_test_scaffolds_body_grows_fence_for_doctests() {
        // Rust doctests embed ```rust...``` inside their source.
        // A fixed-3 fence around the scaffold body would close
        // prematurely on the inner ``` and Forgejo would render
        // the rest as plain text.
        let scaffolds = vec![TestScaffold {
            item_name: "fn frobnicate".into(),
            item_path: "src/lib.rs".into(),
            framework: "cargo-test".into(),
            source: "/// ```rust\n/// frobnicate(1);\n/// ```\nfn frobnicate(x: i32) {}".into(),
        }];
        let body = format_test_scaffolds_body(&scaffolds);
        assert!(
            body.contains("````\n"),
            "expected ≥4-backtick fence around doctest source, got:\n{body}"
        );
    }

    #[test]
    fn format_suggestion_body_emits_suggestion_block_with_reason() {
        let p = AutofixPatch {
            path: "x.rs".into(),
            line: 5,
            replacement: "let x = 1;".into(),
            reason: "Initialize properly.".into(),
        };
        let body = format_suggestion_body(&p);
        assert!(body.starts_with("Initialize properly."));
        assert!(body.contains("```suggestion"));
        assert!(body.contains("let x = 1;"));
    }

    #[test]
    fn format_suggestion_body_handles_missing_trailing_newline() {
        let p = AutofixPatch {
            path: "x.rs".into(),
            line: 1,
            replacement: "no newline".into(),
            reason: "".into(),
        };
        let body = format_suggestion_body(&p);
        // Block must always close on its own line.
        assert!(body.ends_with("```"));
        assert!(body.contains("no newline\n```"));
    }

    #[test]
    fn format_suggestion_body_uses_longer_fence_when_replacement_has_triple_backticks() {
        // Real failure mode: an LLM autofix on a markdown file or a
        // bash heredoc could legitimately contain ``` inside the
        // replacement. With a fixed 3-backtick fence, the inner
        // ``` would close the suggestion block prematurely and
        // Forgejo would render the rest of the comment as plain
        // text — the "Apply suggestion" button might disappear or
        // apply only the truncated body.
        let p = AutofixPatch {
            path: "README.md".into(),
            line: 5,
            replacement: "Heres a code block:\n```rust\nlet x = 1;\n```\nDone.".into(),
            reason: "doc fix".into(),
        };
        let body = format_suggestion_body(&p);
        // The outer fence must be at least 4 backticks since the
        // replacement contains 3.
        assert!(
            body.contains("````suggestion"),
            "expected ≥4-backtick fence, got body:\n{body}"
        );
        // The closing fence must match.
        assert!(body.trim_end().ends_with("````"));
    }

    #[test]
    fn pick_fence_default_is_three_backticks() {
        // Plain replacement with no internal backticks → standard
        // 3-backtick fence (preserves prior behaviour for the
        // common case).
        assert_eq!(pick_fence("plain code"), "```");
        assert_eq!(pick_fence("code with `single` backticks"), "```");
    }

    #[test]
    fn pick_fence_grows_one_longer_than_internal_run() {
        assert_eq!(pick_fence("a ``` b"), "````");
        assert_eq!(pick_fence("a ```` b"), "`````");
    }

    #[test]
    fn longest_backtick_run_counts_consecutive_only() {
        assert_eq!(longest_backtick_run("no backticks"), 0);
        assert_eq!(longest_backtick_run("`a`"), 1);
        assert_eq!(longest_backtick_run("``code``"), 2);
        assert_eq!(longest_backtick_run("```hi``` and ``"), 3);
        // Non-consecutive runs don't merge.
        assert_eq!(longest_backtick_run("`` x ``"), 2);
    }

    /// Contract: every user-facing ChatCommand variant must appear as a
    /// backticked literal in HELP_TEXT. The exhaustive match in
    /// `keyword_for` means adding a new variant fails to compile until
    /// you decide whether it's user-facing and, if so, what string the
    /// help block should advertise.
    #[test]
    fn help_text_documents_every_user_facing_command() {
        fn keyword_for(cmd: &ChatCommand) -> Option<&'static str> {
            match cmd {
                ChatCommand::Help => Some("help"),
                ChatCommand::Remember(_) => Some("remember"),
                ChatCommand::Forget(_) => Some("forget"),
                ChatCommand::ReReview => Some("re-review"),
                ChatCommand::Autofix => Some("autofix"),
                ChatCommand::Docstrings => Some("docstring"),
                ChatCommand::TestScaffolds => Some("tests"),
                ChatCommand::Freeform(_) | ChatCommand::NotMentioned => None,
            }
        }

        let all = [
            ChatCommand::Help,
            ChatCommand::Remember(String::new()),
            ChatCommand::Forget(0),
            ChatCommand::ReReview,
            ChatCommand::Autofix,
            ChatCommand::Docstrings,
            ChatCommand::TestScaffolds,
            ChatCommand::Freeform(String::new()),
            ChatCommand::NotMentioned,
        ];
        for cmd in &all {
            let Some(kw) = keyword_for(cmd) else {
                continue;
            };
            // Each command surfaces in HELP_TEXT either as `keyword`
            // (no args, e.g. "`autofix`") or as `keyword <arg>` (e.g.
            // "`forget <id>`"). Accept either shape.
            let standalone = format!("`{kw}`");
            let with_arg = format!("`{kw} ");
            assert!(
                HELP_TEXT.contains(&standalone) || HELP_TEXT.contains(&with_arg),
                "HELP_TEXT must mention `{kw}` for variant {cmd:?}; \
                 update HELP_TEXT or `keyword_for` so they agree"
            );

            // Round-trip: the keyword we advertise in HELP_TEXT must
            // actually parse to the same variant family (or for the
            // arg-taking ones, to the dispatch help when args are
            // missing). Catches drift like "rename keyword in HELP_TEXT
            // but forget to teach the parser".
            let invocation = match cmd {
                ChatCommand::Remember(_) => format!("@auto_review {kw} sample text"),
                ChatCommand::Forget(_) => format!("@auto_review {kw} 1"),
                _ => format!("@auto_review {kw}"),
            };
            let parsed = crate::parse_chat_command(&invocation, "auto_review");
            let same_family = matches!(
                (cmd, &parsed),
                (ChatCommand::Help, ChatCommand::Help)
                    | (ChatCommand::Remember(_), ChatCommand::Remember(_))
                    | (ChatCommand::Forget(_), ChatCommand::Forget(_))
                    | (ChatCommand::ReReview, ChatCommand::ReReview)
                    | (ChatCommand::Autofix, ChatCommand::Autofix)
                    | (ChatCommand::Docstrings, ChatCommand::Docstrings)
                    | (ChatCommand::TestScaffolds, ChatCommand::TestScaffolds)
            );
            assert!(
                same_family,
                "parser disagrees with HELP_TEXT for `{kw}`: \
                 expected {cmd:?} family, got {parsed:?}"
            );
        }
    }

    #[test]
    fn paths_in_diff_extracts_plus_plus_plus_paths() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n\
                    index abc..def 100644\n\
                    --- a/src/a.rs\n\
                    +++ b/src/a.rs\n\
                    @@ -1 +1 @@\n\
                    -old\n\
                    +new\n";
        let paths = paths_in_diff(diff);
        assert!(paths.contains("src/a.rs"));
    }

    #[test]
    fn paths_in_diff_falls_back_to_diff_git_line() {
        // Some diffs (test fixtures, proxies that strip --- /+++)
        // only have the `diff --git` header. Still extract the path.
        let diff = "diff --git a/src/auth.rs b/src/auth.rs\n+new line\n";
        let paths = paths_in_diff(diff);
        assert!(paths.contains("src/auth.rs"));
    }

    #[test]
    fn paths_in_diff_returns_empty_for_empty_diff() {
        assert!(paths_in_diff("").is_empty());
        assert!(paths_in_diff("no headers here").is_empty());
    }

    #[test]
    fn paths_in_diff_handles_renames() {
        // Rename diff: old path differs from new path; we want the new.
        let diff = "diff --git a/old/x.rs b/new/x.rs\n\
                    similarity index 95%\n\
                    rename from old/x.rs\n\
                    rename to new/x.rs\n\
                    --- a/old/x.rs\n\
                    +++ b/new/x.rs\n";
        let paths = paths_in_diff(diff);
        assert!(paths.contains("new/x.rs"));
    }

    #[test]
    fn paths_in_diff_handles_multiple_files() {
        let diff = "diff --git a/a.rs b/a.rs\n\
                    +++ b/a.rs\n\
                    diff --git a/b.rs b/b.rs\n\
                    +++ b/b.rs\n";
        let paths = paths_in_diff(diff);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("a.rs"));
        assert!(paths.contains("b.rs"));
    }
}
