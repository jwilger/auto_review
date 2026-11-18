//! Bounded JSON-action tool loop shared by agentic callers.
//!
//! Each turn sends the full message history to the configured tier
//! under a strict JSON-schema response format, parses the reply into a
//! closed [`AgentAction`], and either ends the loop (the caller's
//! `on_terminal` recognises the action as final) or runs the named
//! tool from the [`Toolbox`] and appends its result as the next user
//! message.
//!
//! The loop never panics. Every abnormal end is reported as a value so
//! the caller decides its own fail-open policy:
//! - [`LoopEnd::BudgetExhausted`] after `budget` LLM calls with no
//!   terminal action;
//! - [`LoopEnd::Failed`] on an LLM error, a reply that does not parse
//!   as an action, or an unknown tool name.
//!
//! Tool errors (missing arguments, path escapes, bad regexes) are not
//! loop failures: they are reported to the model as the tool result and
//! the loop continues.

use crate::diff::char_boundary_prefix;
use crate::workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError};
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router};
use core::fmt::Write as _;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Cap on the bytes returned to the model from a single `read_file`
/// call. Tunes how aggressively long files get truncated.
const READ_FILE_MAX_BYTES: usize = 4_096;

/// Cap on the number of `search` matches returned to the model.
const SEARCH_MAX_HITS: usize = 64;

/// Cap on the length of a [`LoopEnd::Failed`] message. Error text can
/// carry provider bodies or model-chosen strings; keep log lines small.
const FAILED_MESSAGE_CAP: usize = 1024;

/// One model action, parsed with serde from the model's JSON reply.
///
/// Closed struct: only these fields are read. Extra fields the schema
/// allows (for example a verdict's `reason`) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AgentAction {
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub keep: Option<bool>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub start_line: Option<u32>,
    pub tool: String,
}

/// How a loop run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopEnd<T> {
    /// `budget` LLM calls ran without a terminal action.
    BudgetExhausted,
    /// The LLM call failed, the reply was not a valid action, or the
    /// action named an unknown tool. The message is length-capped.
    Failed(String),
    /// `on_terminal` accepted an action and produced this value.
    Terminal(T),
}

/// Counters for one loop run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoopStats {
    /// Tools executed.
    pub tool_calls: usize,
    /// LLM calls made.
    pub turns: usize,
}

/// Result of [`run_agent_loop`]: how it ended, the final message
/// history and the run's counters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LoopOutcome<T> {
    pub end: LoopEnd<T>,
    pub messages: Vec<Message>,
    pub stats: LoopStats,
}

/// Read-only tools the loop can run, bounded to one workspace root.
#[derive(Debug, Clone)]
pub struct Toolbox {
    /// Workspace root every tool call is confined to.
    root: PathBuf,
}

impl Toolbox {
    /// Tools bounded to the workspace at `root`.
    #[inline]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Workspace root every tool call is confined to.
    #[inline]
    #[must_use = "reading the root has no side effects"]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run the tool `action.tool` names and format its result for the
    /// model. `None` means the tool is unknown.
    fn run(&self, action: &AgentAction) -> Option<String> {
        match action.tool.as_str() {
            "read_file" => {
                let result = action.path.as_deref().map(|rel_path| {
                    read_file(
                        &self.root,
                        rel_path,
                        action.start_line,
                        action.end_line,
                        READ_FILE_MAX_BYTES,
                    )
                });
                Some(format_read_result(action.path.as_deref(), result))
            }
            "search" => {
                let result = action.pattern.as_deref().map(|pattern| {
                    search(&self.root, pattern, action.path.as_deref(), SEARCH_MAX_HITS)
                });
                Some(format_search_result(
                    action.pattern.as_deref(),
                    action.path.as_deref(),
                    result,
                ))
            }
            _ => None,
        }
    }
}

/// Run a bounded JSON-action tool loop.
///
/// Every turn sends one `router.complete(tier, ..)` with `system`, the
/// full history, `action_schema` as a strict JSON-schema response
/// format named `schema_name`, and temperature 0. The reply is parsed
/// into an [`AgentAction`] and appended to the history as an assistant
/// message. `on_terminal` then decides: `Some(t)` ends the loop with
/// [`LoopEnd::Terminal`]; `None` runs the action's tool and appends the
/// result as a user message.
///
/// `budget` caps the number of LLM calls.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameter list is the shared contract every agentic caller passes through; \
              bundling it into a struct would only move the same fields one level down"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "agentic_verify calls this by name as run_agent_loop; a bare `run` would not say \
              which loop runs at that call site"
)]
#[inline]
pub async fn run_agent_loop<T, F>(
    router: &Router,
    tier: ModelTier,
    system: &str,
    mut messages: Vec<Message>,
    schema_name: &str,
    action_schema: serde_json::Value,
    toolbox: &Toolbox,
    budget: usize,
    mut on_terminal: F,
) -> LoopOutcome<T>
where
    F: FnMut(&AgentAction) -> Option<T>,
{
    let mut stats = LoopStats::default();

    while stats.turns < budget {
        stats.turns = stats.turns.saturating_add(1);
        let req = CompleteRequest {
            system: Some(system.to_owned()),
            messages: messages.clone(),
            response_format: Some(ResponseFormat::JsonSchema {
                name: schema_name.to_owned(),
                schema: action_schema.clone(),
            }),
            // Determinism: repeated runs over the same input should
            // not randomly take different paths.
            temperature: Some(0.0),
            ..Default::default()
        };
        let resp = match router.complete(tier, req).await {
            Ok(response) => response,
            Err(err) => return failed(&format!("LLM call failed: {err}"), messages, stats),
        };
        let action: AgentAction = match serde_json::from_str(&resp.content) {
            Ok(parsed) => parsed,
            Err(err) => return failed(&format!("malformed action JSON: {err}"), messages, stats),
        };
        messages.push(Message::assistant(resp.content));

        if let Some(value) = on_terminal(&action) {
            return LoopOutcome {
                end: LoopEnd::Terminal(value),
                messages,
                stats,
            };
        }
        match toolbox.run(&action) {
            Some(result) => {
                stats.tool_calls = stats.tool_calls.saturating_add(1);
                messages.push(Message::user(result));
            }
            None => {
                return failed(&format!("unknown tool: {}", action.tool), messages, stats);
            }
        }
    }

    LoopOutcome {
        end: LoopEnd::BudgetExhausted,
        messages,
        stats,
    }
}

/// End the loop as [`LoopEnd::Failed`] with a length-capped `msg`.
fn failed<T>(msg: &str, messages: Vec<Message>, stats: LoopStats) -> LoopOutcome<T> {
    LoopOutcome {
        end: LoopEnd::Failed(cap_message(msg)),
        messages,
        stats,
    }
}

/// Same cap as `ar_llm`'s provider-error cap: keep the first
/// [`FAILED_MESSAGE_CAP`] bytes on a UTF-8 boundary and note the rest.
fn cap_message(text: &str) -> String {
    if text.len() <= FAILED_MESSAGE_CAP {
        return text.to_owned();
    }
    let prefix = char_boundary_prefix(text, FAILED_MESSAGE_CAP);
    format!(
        "{prefix}… [+{} bytes]",
        text.len().saturating_sub(prefix.len())
    )
}

/// Format a `read_file` tool result (or its argument error) for the model.
fn format_read_result(
    path: Option<&str>,
    result: Option<Result<ReadResult, WorkspaceToolError>>,
) -> String {
    match (path, result) {
        (None, _) => "Tool error: read_file requires a 'path' argument.".into(),
        (Some(rel_path), Some(Ok(read))) => {
            let mut text = format!(
                "read_file({rel_path}) [lines {}-{}{}]:\n```\n",
                read.start_line,
                read.end_line,
                if read.truncated { ", truncated" } else { "" }
            );
            text.push_str(&read.content);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("```");
            text
        }
        (Some(rel_path), Some(Err(err))) => format!("read_file({rel_path}) error: {err}"),
        (Some(rel_path), None) => {
            format!("read_file({rel_path}): unreachable (no path resolution)")
        }
    }
}

/// Format a `search` tool result (or its argument error) for the model.
fn format_search_result(
    pattern: Option<&str>,
    path: Option<&str>,
    result: Option<Result<Vec<SearchHit>, WorkspaceToolError>>,
) -> String {
    let Some(pattern_text) = pattern else {
        return "Tool error: search requires a 'pattern' argument.".into();
    };
    let path_label = path.unwrap_or("<workspace>");
    match result {
        Some(Ok(hits)) if hits.is_empty() => {
            format!("search(/{pattern_text}/, {path_label}): no matches.")
        }
        Some(Ok(hits)) => {
            let mut text = format!(
                "search(/{pattern_text}/, {path_label}): {} match(es).\n",
                hits.len()
            );
            for hit in hits {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "fmt::Write for String never returns Err, so there is no error to \
                              handle"
                )]
                let _ = writeln!(text, "  {}:{}: {}", hit.path, hit.line, hit.line_text);
            }
            text
        }
        Some(Err(err)) => format!("search(/{pattern_text}/, {path_label}) error: {err}"),
        None => "search: pattern resolution failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use ar_llm::testing::ScriptedProvider;
    use ar_llm::Role;
    use serde_json::json;

    const SYSTEM: &str = "system prompt";

    fn schema() -> serde_json::Value {
        json!({"type": "object", "properties": {"tool": {"type": "string"}}})
    }

    fn router(provider: &Arc<ScriptedProvider>) -> Router {
        Router::new().with(ModelTier::Cheap, Arc::<ScriptedProvider>::clone(provider))
    }

    /// `verdict` is terminal; everything else goes to the toolbox.
    fn verdict(action: &AgentAction) -> Option<bool> {
        (action.tool == "verdict").then(|| action.keep.unwrap_or(true))
    }

    async fn run(
        provider: &Arc<ScriptedProvider>,
        toolbox: &Toolbox,
        budget: usize,
    ) -> LoopOutcome<bool> {
        run_agent_loop(
            &router(provider),
            ModelTier::Cheap,
            SYSTEM,
            vec![Message::user("start")],
            "Action",
            schema(),
            toolbox,
            budget,
            verdict,
        )
        .await
    }

    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn assert_role(m: &Message, role: Role) {
        assert_eq!(m.role, role);
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::pattern_type_mismatch,
        reason = "the test matches borrowed fixtures and recorded values with default binding modes; explicit & patterns would edit the matched shapes the assertions read"
    )]
    async fn each_turn_sends_full_history_under_strict_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "hello\n").expect("write");
        let turn1 = r#"{"tool":"read_file","path":"a.txt"}"#;
        let provider = Arc::new(ScriptedProvider::new(vec![
            turn1,
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        assert_eq!(out.end, LoopEnd::Terminal(true));

        let reqs = provider.requests();
        assert_eq!(reqs.len(), 2);
        for req in &reqs {
            assert_eq!(req.system.as_deref(), Some(SYSTEM));
            assert_eq!(req.temperature, Some(0.0));
            assert_eq!(req.max_tokens, None);
            match &req.response_format {
                Some(ResponseFormat::JsonSchema { name, schema: s }) => {
                    assert_eq!(name, "Action");
                    assert_eq!(s, &schema());
                }
                other => panic!("unexpected response format: {other:?}"),
            }
        }
        assert_eq!(reqs[0].messages.len(), 1);
        let second = &reqs[1].messages;
        assert_eq!(second.len(), 3);
        assert_role(&second[0], Role::User);
        assert_eq!(second[0].content, "start");
        assert_role(&second[1], Role::Assistant);
        assert_eq!(second[1].content, turn1);
        assert_role(&second[2], Role::User);
        assert!(second[2]
            .content
            .starts_with("read_file(a.txt) [lines 1-1]"));
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn terminal_first_response_ends_without_running_a_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"verdict","keep":false}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        assert_eq!(out.end, LoopEnd::Terminal(false));
        assert_eq!(
            out.stats,
            LoopStats {
                turns: 1,
                tool_calls: 0
            }
        );
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    async fn read_file_tool_result_is_sent_back_as_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.rs"), "line1\nline2\n").expect("write");
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"read_file","path":"f.rs","start_line":2,"end_line":2}"#,
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        assert_eq!(out.stats.tool_calls, 1);
        let tool_msg = &out.messages[2];
        assert_role(tool_msg, Role::User);
        assert_eq!(
            tool_msg.content,
            "read_file(f.rs) [lines 2-2]:\n```\nline2\n```"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    async fn tool_error_is_reported_and_loop_continues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"read_file"}"#,
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        assert_eq!(out.end, LoopEnd::Terminal(true));
        assert_eq!(
            out.messages[2].content,
            "Tool error: read_file requires a 'path' argument."
        );
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn budget_caps_llm_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("x.rs"), "x\n").expect("write");
        let read = r#"{"tool":"read_file","path":"x.rs"}"#;
        // A fourth response is scripted so an extra call would succeed
        // rather than fail; the loop must not make it.
        let provider = Arc::new(ScriptedProvider::new(vec![read, read, read, read]));
        let out = run(&provider, &Toolbox::new(dir.path()), 3).await;
        assert_eq!(out.end, LoopEnd::BudgetExhausted);
        assert_eq!(out.stats.turns, 3);
        assert_eq!(provider.call_count(), 3);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: any other variant fails the test through the wildcard arm"
    )]
    async fn llm_error_fails_without_further_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        match out.end {
            LoopEnd::Failed(msg) => assert!(msg.contains("no scripted responses left")),
            other => panic!("unexpected end: {other:?}"),
        }
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn malformed_json_fails_without_further_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = Arc::new(ScriptedProvider::new(vec![
            "not json",
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        assert!(matches!(out.end, LoopEnd::Failed(_)));
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "test code: any other variant fails the test through the wildcard arm"
    )]
    async fn unknown_tool_fails_without_further_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"hack","path":"/etc/passwd"}"#,
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let out = run(&provider, &Toolbox::new(dir.path()), 5).await;
        match out.end {
            LoopEnd::Failed(msg) => assert!(msg.contains("unknown tool: hack")),
            other => panic!("unexpected end: {other:?}"),
        }
        assert_eq!(out.stats.tool_calls, 0);
        assert_eq!(provider.call_count(), 1);
    }

    #[test]
    #[expect(
        clippy::non_ascii_literal,
        reason = "the fixture and expected text hold the exact non-ASCII characters (em dash, ellipsis, emoji, multi-byte letters) the code under test renders or must cut on a character boundary; escapes would edit what the test checks"
    )]
    fn cap_message_truncates_long_text_on_char_boundary() {
        let long = "é".repeat(FAILED_MESSAGE_CAP);
        let capped = cap_message(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("… [+"));
    }
}
