//! Agentic verifier: per-finding ReAct loop with read-only workspace
//! tools.
//!
//! Where [`crate::verify::verify_findings`] does a single-pass yes/no
//! judgement against the diff, the agentic verifier gives the cheap
//! model a budget of tool calls — `read_file` and `search` against
//! the cloned workspace — so it can pull additional context the diff
//! alone doesn't show. Useful when a finding cites code the diff
//! doesn't include verbatim (e.g. "function X has a bug, but X is
//! defined in unchanged code").
//!
//! Per-finding loop:
//! 1. Render system + initial-user prompt with the finding +
//!    surrounding diff + tool catalog.
//! 2. Call cheap-tier LLM with a strict JSON-schema constraint over
//!    `{tool: "read_file" | "search" | "verdict", ...}`.
//! 3. Execute the tool against [`crate::workspace_tools`] (read-only,
//!    workspace-bounded).
//! 4. Append the tool result as the next user message and loop until
//!    the model emits a `verdict` or the turn budget is exhausted.
//!
//! Bounded turn budget per finding (default 5). Fails open: any error
//! (LLM failure, malformed JSON, tool error, budget exhausted) keeps
//! the finding rather than dropping it.

use crate::error::ReviewError;
use crate::workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError};
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router};
use ar_prompts::{ReviewFinding, ReviewOutput};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;
use std::path::Path;

/// Maximum number of tool calls the verifier is allowed to make per
/// finding before it must emit a verdict.
const DEFAULT_MAX_TURNS: usize = 5;

/// Cap on the bytes returned to the model from a single `read_file`
/// call. Tunes how aggressively long files get truncated.
const READ_FILE_MAX_BYTES: usize = 4_096;

/// Cap on the number of `search` matches returned to the model.
const SEARCH_MAX_HITS: usize = 64;

const SYSTEM_PROMPT: &str = r#"You are an agentic verifier of code-review findings. You will receive
one finding at a time, plus the unified diff that produced it. Your job
is to decide whether the finding is corroborated by the actual code in
the workspace — drop spurious findings, keep real ones.

You can issue these tool calls (one per turn):

- {"tool":"read_file","path":"<repo-relative path>","start_line":<int?>,"end_line":<int?>}
  Read part or all of a file. Lines are 1-indexed inclusive. Both line
  fields are optional; omit them to read the whole file (capped).

- {"tool":"search","pattern":"<regex>","path":"<repo-relative path?>"}
  Regex-search either a single file or the whole workspace (omit path).
  Returns lines that match.

- {"tool":"verdict","keep":<bool>,"reason":"<short rationale>"}
  Final answer. The loop ends here.

Return EXACTLY ONE JSON object per turn. No markdown. No prose. The
schema is validated; malformed output is treated as a failed turn and
the finding is kept by default.

Default to keep=true unless your investigation actively contradicts
the finding. False positives at the verifier hurt review quality less
than false negatives (dropping real bugs).
"#;

#[derive(Debug, Deserialize)]
struct AgentAction {
    tool: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    keep: Option<bool>,
    // The LLM also emits a `reason` field per the prompt; we accept
    // it (extra fields are ignored by serde) but don't need it on
    // our side — the verdict is the only thing the loop cares about.
}

fn agent_action_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "tool": {"type": "string", "enum": ["read_file", "search", "verdict"]},
            "path": {"type": "string"},
            "pattern": {"type": "string"},
            "start_line": {"type": "integer", "minimum": 1},
            "end_line": {"type": "integer", "minimum": 1},
            "keep": {"type": "boolean"},
            "reason": {"type": "string"}
        },
        "required": ["tool"],
        "additionalProperties": false
    })
}

/// Per-finding outcome the loop converges to.
enum Verdict {
    Keep,
    Drop,
}

/// Run the agentic verifier over `output.findings`. Returns a new
/// [`ReviewOutput`] with only the findings the verifier corroborates.
///
/// Behavior when the verifier can't run (no Cheap tier configured,
/// any per-finding loop error): the finding stays. Verifier failures
/// must not silently drop real findings.
pub async fn verify_findings_agentic(
    router: &Router,
    output: ReviewOutput,
    workspace_path: &Path,
    diff: &str,
) -> Result<ReviewOutput, ReviewError> {
    if router.provider(ModelTier::Cheap).is_err() || output.findings.is_empty() {
        return Ok(output);
    }

    let ReviewOutput {
        summary,
        walkthrough,
        mermaid,
        findings,
    } = output;

    let mut kept: Vec<ReviewFinding> = Vec::with_capacity(findings.len());
    let mut dropped = 0usize;
    for finding in findings {
        match verify_one(router, &finding, workspace_path, diff, DEFAULT_MAX_TURNS).await {
            Verdict::Keep => kept.push(finding),
            Verdict::Drop => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::info!(
            dropped,
            kept = kept.len(),
            "agentic verifier dropped suspect findings"
        );
    }

    Ok(ReviewOutput {
        summary,
        walkthrough,
        mermaid,
        findings: kept,
    })
}

async fn verify_one(
    router: &Router,
    finding: &ReviewFinding,
    workspace_path: &Path,
    diff: &str,
    max_turns: usize,
) -> Verdict {
    let mut messages = vec![Message::user(initial_user_prompt(finding, diff))];

    for _ in 0..max_turns {
        let req = CompleteRequest {
            system: Some(SYSTEM_PROMPT.to_string()),
            messages: messages.clone(),
            response_format: Some(ResponseFormat::JsonSchema {
                name: "AgentAction".to_string(),
                schema: agent_action_schema(),
            }),
            ..Default::default()
        };
        let resp = match router.complete(ModelTier::Cheap, req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "agentic verifier LLM call failed; keeping finding");
                return Verdict::Keep;
            }
        };
        let action: AgentAction = match serde_json::from_str(&resp.content) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "agentic verifier emitted malformed JSON; keeping finding");
                return Verdict::Keep;
            }
        };
        messages.push(Message::assistant(resp.content));

        match action.tool.as_str() {
            "verdict" => {
                let keep = action.keep.unwrap_or(true);
                return if keep { Verdict::Keep } else { Verdict::Drop };
            }
            "read_file" => {
                let result = action.path.as_deref().map(|p| {
                    read_file(
                        workspace_path,
                        p,
                        action.start_line,
                        action.end_line,
                        READ_FILE_MAX_BYTES,
                    )
                });
                let formatted = format_read_result(action.path.as_deref(), result);
                messages.push(Message::user(formatted));
            }
            "search" => {
                let result = action
                    .pattern
                    .as_deref()
                    .map(|p| search(workspace_path, p, action.path.as_deref(), SEARCH_MAX_HITS));
                let formatted =
                    format_search_result(action.pattern.as_deref(), action.path.as_deref(), result);
                messages.push(Message::user(formatted));
            }
            other => {
                tracing::warn!(
                    tool = other,
                    "agentic verifier issued unknown tool; keeping finding"
                );
                return Verdict::Keep;
            }
        }
    }

    tracing::info!(
        path = finding.path,
        line = finding.line_start,
        "agentic verifier exhausted turn budget; keeping finding"
    );
    let _ = action_for_logging(&messages);
    Verdict::Keep
}

fn action_for_logging(messages: &[Message]) -> Option<&str> {
    messages
        .last()
        .filter(|m| matches!(m.role, ar_llm::Role::Assistant))
        .map(|m| m.content.as_str())
}

/// Same cheap-tier context-budget rationale as
/// `verify::VERIFY_DIFF_CAP`. The agentic verifier embeds the diff
/// once per finding and walks tool calls — each turn re-sends the
/// growing message vec, so an oversized initial prompt compounds.
const AGENTIC_DIFF_CAP: usize = 40 * 1024;

fn initial_user_prompt(finding: &ReviewFinding, diff: &str) -> String {
    let mut out = String::with_capacity(diff.len().min(AGENTIC_DIFF_CAP) + 512);
    out.push_str("Finding to verify:\n");
    let _ = writeln!(
        out,
        "  path: {}\n  line_start: {}\n  severity: {:?}\n  message: {}",
        finding.path, finding.line_start, finding.severity, finding.message
    );
    out.push_str("\nUnified diff:\n```diff\n");
    if diff.len() <= AGENTIC_DIFF_CAP {
        out.push_str(diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
    } else {
        let mut cut = AGENTIC_DIFF_CAP;
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push_str(&diff[..cut]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("[diff truncated for agentic verifier]\n");
    }
    out.push_str("```\n\nIssue your first tool call now.\n");
    out
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
    use ar_llm::{CompleteResponse, Error as LlmError, LlmProvider};
    use ar_prompts::ReviewSeverity;
    use async_trait::async_trait;
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};

    /// Provider that returns canned responses LIFO from a stack.
    /// Lets a test script a multi-turn conversation as a list of
    /// (in order) JSON strings.
    struct ScriptedProvider {
        responses: Mutex<Vec<String>>,
    }

    impl ScriptedProvider {
        fn new(mut responses: Vec<&str>) -> Self {
            // Reverse so we can pop() in order.
            responses.reverse();
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            let content = self
                .responses
                .lock()
                .unwrap()
                .pop()
                .expect("scripted provider ran out of responses");
            Ok(CompleteResponse {
                content,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn finding(i: u32, msg: &str, path: &str) -> ReviewFinding {
        ReviewFinding {
            path: path.into(),
            line_start: i,
            line_end: None,
            severity: ReviewSeverity::Warning,
            message: msg.into(),
        }
    }

    fn output(findings: Vec<ReviewFinding>) -> ReviewOutput {
        ReviewOutput {
            summary: "s".into(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings,
        }
    }

    #[tokio::test]
    async fn returns_input_unchanged_when_cheap_tier_missing() {
        let dir = tempfile::tempdir().unwrap();
        let router = Router::new();
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "x", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn returns_input_unchanged_when_findings_empty() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(&router, output(vec![]), dir.path(), "diff")
            .await
            .expect("ok");
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    async fn drops_finding_when_verdict_says_keep_false() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"verdict","keep":false,"reason":"spurious"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "bad", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    async fn keeps_finding_when_verdict_says_keep_true() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"verdict","keep":true,"reason":"confirmed"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "real", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn read_file_action_then_verdict_drops_finding() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/foo.rs", "line1\nline2\nline3\n");
        // Turn 1: read_file. Turn 2: verdict drop.
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"read_file","path":"src/foo.rs","start_line":1,"end_line":3}"#,
            r#"{"tool":"verdict","keep":false,"reason":"after reading, finding doesn't apply"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(2, "questionable", "src/foo.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    async fn search_action_then_verdict_keeps_finding() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/auth.rs",
            "fn validate() {\n    panic!();\n}\n",
        );
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"search","pattern":"panic!","path":"src/auth.rs"}"#,
            r#"{"tool":"verdict","keep":true,"reason":"panic confirmed at line 2"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(2, "panic in handler", "src/auth.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn malformed_json_keeps_finding() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec!["this is not json"]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "x", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        // Fail-open: real findings preserved.
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn unknown_tool_keeps_finding() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"hack","path":"/etc/passwd"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "x", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn turn_budget_exhausted_keeps_finding() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.rs", "x\n");
        // Five identical read_file calls without a verdict — exceeds
        // DEFAULT_MAX_TURNS = 5 and the loop falls open.
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"read_file","path":"x.rs"}"#,
            r#"{"tool":"read_file","path":"x.rs"}"#,
            r#"{"tool":"read_file","path":"x.rs"}"#,
            r#"{"tool":"read_file","path":"x.rs"}"#,
            r#"{"tool":"read_file","path":"x.rs"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "y", "x.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        // Fail-open: keep the finding.
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn read_file_path_escape_is_a_tool_error_finding_kept() {
        let dir = tempfile::tempdir().unwrap();
        // First turn: try to escape. Second turn: verdict keep
        // (model decides based on the error).
        let provider = Arc::new(ScriptedProvider::new(vec![
            r#"{"tool":"read_file","path":"../etc/passwd"}"#,
            r#"{"tool":"verdict","keep":true,"reason":"can't access; assume real"}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![finding(1, "x", "a.rs")]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        // The path-escape error is reported as a tool message; the
        // model still gets to choose keep on the next turn.
        assert_eq!(result.findings.len(), 1);
    }

    #[tokio::test]
    async fn per_finding_independence_kept_and_dropped_findings_split() {
        let dir = tempfile::tempdir().unwrap();
        // Two findings: first dropped, second kept.
        let provider = Arc::new(ScriptedProvider::new(vec![
            // For finding 0:
            r#"{"tool":"verdict","keep":false}"#,
            // For finding 1:
            r#"{"tool":"verdict","keep":true}"#,
        ]));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings_agentic(
            &router,
            output(vec![
                finding(1, "drop me", "a.rs"),
                finding(2, "keep me", "b.rs"),
            ]),
            dir.path(),
            "diff",
        )
        .await
        .expect("ok");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path, "b.rs");
    }
}
