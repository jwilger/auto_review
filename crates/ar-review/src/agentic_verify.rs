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

use crate::agent_loop::{run_agent_loop, LoopEnd, Toolbox};
use crate::error::ReviewError;
use ar_llm::{Message, ModelTier, Router};
use ar_prompts::{ReviewFinding, ReviewOutput};
use serde_json::json;
use std::fmt::Write;
use std::path::Path;

/// Maximum number of tool calls the verifier is allowed to make per
/// finding before it must emit a verdict.
const DEFAULT_MAX_TURNS: usize = 5;

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
    let toolbox = Toolbox::new(workspace_path);
    let outcome = run_agent_loop(
        router,
        ModelTier::Cheap,
        SYSTEM_PROMPT,
        vec![Message::user(initial_user_prompt(finding, diff))],
        "AgentAction",
        agent_action_schema(),
        &toolbox,
        max_turns,
        |action| (action.tool == "verdict").then(|| action.keep.unwrap_or(true)),
    )
    .await;

    match outcome.end {
        LoopEnd::Terminal(true) => Verdict::Keep,
        LoopEnd::Terminal(false) => Verdict::Drop,
        LoopEnd::BudgetExhausted => {
            tracing::info!(
                path = finding.path,
                line = finding.line_start,
                "agentic verifier exhausted turn budget; keeping finding"
            );
            Verdict::Keep
        }
        LoopEnd::Failed(error) => {
            tracing::warn!(%error, "agentic verifier failed; keeping finding");
            Verdict::Keep
        }
    }
}

fn initial_user_prompt(finding: &ReviewFinding, diff: &str) -> String {
    let capped = crate::diff::cap_for_prompt(
        diff,
        crate::diff::CHEAP_TIER_DIFF_CAP,
        "[diff truncated for agentic verifier]",
    );
    let mut out = String::with_capacity(capped.len() + 512);
    out.push_str("Finding to verify:\n");
    let _ = writeln!(
        out,
        "  path: {}\n  line_start: {}\n  severity: {:?}\n  message: {}",
        finding.path, finding.line_start, finding.severity, finding.message
    );
    out.push_str("\nUnified diff:\n```diff\n");
    out.push_str(&capped);
    if !capped.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n\nIssue your first tool call now.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::testing::ScriptedProvider;
    use ar_prompts::ReviewSeverity;
    use std::io::Write as _;
    use std::sync::Arc;

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
