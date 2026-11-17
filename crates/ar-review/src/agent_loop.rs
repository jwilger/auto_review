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

use crate::workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError};
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router};
use serde::Deserialize;
use std::fmt::Write;
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
pub struct AgentAction {
    pub tool: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub start_line: Option<u32>,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub keep: Option<bool>,
}

/// How a loop run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopEnd<T> {
    /// `on_terminal` accepted an action and produced this value.
    Terminal(T),
    /// `budget` LLM calls ran without a terminal action.
    BudgetExhausted,
    /// The LLM call failed, the reply was not a valid action, or the
    /// action named an unknown tool. The message is length-capped.
    Failed(String),
}

/// Counters for one loop run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopStats {
    /// LLM calls made.
    pub turns: usize,
    /// Tools executed.
    pub tool_calls: usize,
}

/// Result of [`run_agent_loop`]: how it ended, the final message
/// history and the run's counters.
#[derive(Debug, Clone)]
pub struct LoopOutcome<T> {
    pub end: LoopEnd<T>,
    pub messages: Vec<Message>,
    pub stats: LoopStats,
}

/// Read-only tools the loop can run, bounded to one workspace root.
#[derive(Debug, Clone)]
pub struct Toolbox {
    root: PathBuf,
}

impl Toolbox {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run the tool `action.tool` names and format its result for the
    /// model. `None` means the tool is unknown.
    fn run(&self, action: &AgentAction) -> Option<String> {
        match action.tool.as_str() {
            "read_file" => {
                let result = action.path.as_deref().map(|p| {
                    read_file(
                        &self.root,
                        p,
                        action.start_line,
                        action.end_line,
                        READ_FILE_MAX_BYTES,
                    )
                });
                Some(format_read_result(action.path.as_deref(), result))
            }
            "search" => {
                let result = action
                    .pattern
                    .as_deref()
                    .map(|p| search(&self.root, p, action.path.as_deref(), SEARCH_MAX_HITS));
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
// The parameter list is the shared contract every agentic caller
// passes through; bundling it into a struct would only move the same
// fields one level down.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop<T, F>(
    router: &Router,
    tier: ModelTier,
    system: &str,
    messages: Vec<Message>,
    schema_name: &str,
    action_schema: serde_json::Value,
    toolbox: &Toolbox,
    budget: usize,
    mut on_terminal: F,
) -> LoopOutcome<T>
where
    F: FnMut(&AgentAction) -> Option<T>,
{
    let mut messages = messages;
    let mut stats = LoopStats::default();

    while stats.turns < budget {
        stats.turns += 1;
        let req = CompleteRequest {
            system: Some(system.to_string()),
            messages: messages.clone(),
            response_format: Some(ResponseFormat::JsonSchema {
                name: schema_name.to_string(),
                schema: action_schema.clone(),
            }),
            // Determinism: repeated runs over the same input should
            // not randomly take different paths.
            temperature: Some(0.0),
            ..Default::default()
        };
        let resp = match router.complete(tier, req).await {
            Ok(r) => r,
            Err(e) => return failed(format!("LLM call failed: {e}"), messages, stats),
        };
        let action: AgentAction = match serde_json::from_str(&resp.content) {
            Ok(a) => a,
            Err(e) => return failed(format!("malformed action JSON: {e}"), messages, stats),
        };
        messages.push(Message::assistant(resp.content));

        if let Some(t) = on_terminal(&action) {
            return LoopOutcome {
                end: LoopEnd::Terminal(t),
                messages,
                stats,
            };
        }
        match toolbox.run(&action) {
            Some(result) => {
                stats.tool_calls += 1;
                messages.push(Message::user(result));
            }
            None => {
                return failed(format!("unknown tool: {}", action.tool), messages, stats);
            }
        }
    }

    LoopOutcome {
        end: LoopEnd::BudgetExhausted,
        messages,
        stats,
    }
}

fn failed<T>(msg: String, messages: Vec<Message>, stats: LoopStats) -> LoopOutcome<T> {
    LoopOutcome {
        end: LoopEnd::Failed(cap_message(&msg)),
        messages,
        stats,
    }
}

/// Same cap as `ar_llm`'s provider-error cap: keep the first
/// [`FAILED_MESSAGE_CAP`] bytes on a UTF-8 boundary and note the rest.
fn cap_message(s: &str) -> String {
    if s.len() <= FAILED_MESSAGE_CAP {
        return s.to_string();
    }
    let mut cut = FAILED_MESSAGE_CAP;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [+{} bytes]", &s[..cut], s.len() - cut)
}

fn format_read_result(
    path: Option<&str>,
    result: Option<Result<ReadResult, WorkspaceToolError>>,
) -> String {
    match (path, result) {
        (None, _) => "Tool error: read_file requires a 'path' argument.".into(),
        (Some(p), Some(Ok(r))) => {
            let mut s = format!(
                "read_file({p}) [lines {}-{}{}]:\n```\n",
                r.start_line,
                r.end_line,
                if r.truncated { ", truncated" } else { "" }
            );
            s.push_str(&r.content);
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("```");
            s
        }
        (Some(p), Some(Err(e))) => format!("read_file({p}) error: {e}"),
        (Some(p), None) => format!("read_file({p}): unreachable (no path resolution)"),
    }
}

fn format_search_result(
    pattern: Option<&str>,
    path: Option<&str>,
    result: Option<Result<Vec<SearchHit>, WorkspaceToolError>>,
) -> String {
    let pattern = match pattern {
        Some(p) => p,
        None => return "Tool error: search requires a 'pattern' argument.".into(),
    };
    let path_label = path.unwrap_or("<workspace>");
    match result {
        Some(Ok(hits)) if hits.is_empty() => {
            format!("search(/{pattern}/, {path_label}): no matches.")
        }
        Some(Ok(hits)) => {
            let mut s = format!(
                "search(/{pattern}/, {path_label}): {} match(es).\n",
                hits.len()
            );
            for hit in hits {
                let _ = writeln!(s, "  {}:{}: {}", hit.path, hit.line, hit.line_text);
            }
            s
        }
        Some(Err(e)) => format!("search(/{pattern}/, {path_label}) error: {e}"),
        None => "search: pattern resolution failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::testing::ScriptedProvider;
    use ar_llm::Role;
    use serde_json::json;
    use std::sync::Arc;

    const SYSTEM: &str = "system prompt";

    fn schema() -> serde_json::Value {
        json!({"type": "object", "properties": {"tool": {"type": "string"}}})
    }

    fn router(provider: &Arc<ScriptedProvider>) -> Router {
        Router::new().with(ModelTier::Cheap, provider.clone())
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

    fn assert_role(m: &Message, role: Role) {
        assert_eq!(m.role, role);
    }

    #[tokio::test]
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
    fn cap_message_truncates_long_text_on_char_boundary() {
        let long = "é".repeat(FAILED_MESSAGE_CAP);
        let capped = cap_message(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("… [+"));
    }
}
