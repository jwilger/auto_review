//! Red-team integration tests for the review-pipeline mitigations
//! enumerated in `docs/THREAT-MODEL.md`.
//!
//! Each test corresponds to one threat (T#) and exercises the
//! mitigation described in the threat model. The file doubles as
//! a security audit lens: a reviewer reading just this file sees
//! concretely what we defend against.
//!
//! T1 (Kudelski-class tool-config execution): not exercised here —
//!   the mitigation is architectural. Normal review jobs do not run
//!   repo-controlled deterministic tools; workspace Git hardening is
//!   tested in `crates/ar-review/src/workspace.rs`.
//! T2 (webhook forgery): exercised in
//!   `crates/ar-gateway/src/webhook.rs` HMAC unit tests.
//! T3 (prompt injection in PR body): tested below.
//! T4 (LLM tool calls escape workspace): exercised in
//!   `crates/ar-review/tests/red_team_workspace_tools.rs`.
//! T5 (PAT compromise): operational; exercised by the redactor
//!   tests in `workspace.rs`.
//! T7 (resource exhaustion via huge diff): tested below.
//! T8 (token-cost amplification): tested below — same defence as
//!   T7 (the diff cap), but the test framing is different.
//! T9 (confused-deputy via Forgejo API): tested below.

use ar_prompts::{validate_review_output, ReviewSeverity};
use ar_review::{cap_diff, output_to_review_request, DEFAULT_MAX_DIFF_BYTES};

/// T7 mitigation: diffs above the byte cap are truncated at file
/// boundaries with an "omitted N file(s)" marker. A 50 MiB diff
/// must NOT be passed verbatim to the LLM (token cost +
/// context-window blow-up).
#[test]
fn t7_oversized_diff_is_capped_at_file_boundaries() {
    // Build a diff that's 50 files × ~200 KiB each ≈ 10 MiB total.
    // Way above DEFAULT_MAX_DIFF_BYTES (100 KiB).
    let mut huge = String::new();
    for i in 0..50 {
        huge.push_str(&format!(
            "diff --git a/file{i}.rs b/file{i}.rs\n--- a/file{i}.rs\n+++ b/file{i}.rs\n@@ -1 +1 @@\n",
        ));
        huge.push_str(&"+".repeat(200_000));
        huge.push('\n');
    }
    let capped = cap_diff(&huge, DEFAULT_MAX_DIFF_BYTES);
    assert!(
        capped.len() <= DEFAULT_MAX_DIFF_BYTES + 200,
        "capped len = {}",
        capped.len()
    );
    assert!(capped.contains("omitted"), "expected omission marker");
}

/// T8 mitigation: a single oversized file (no `diff --git`
/// boundary to split on) falls back to flat truncation rather
/// than overflowing the LLM context. Operators may worry the
/// limit silently breaks reviewing big files; this test pins the
/// fallback behaviour.
#[test]
fn t8_oversized_single_file_falls_back_to_flat_truncation() {
    // No `diff --git` markers — just a giant blob.
    let huge = "x".repeat(2 * DEFAULT_MAX_DIFF_BYTES);
    let capped = cap_diff(&huge, DEFAULT_MAX_DIFF_BYTES);
    assert!(capped.len() <= DEFAULT_MAX_DIFF_BYTES + 200);
    assert!(capped.contains("truncated"));
}

/// T9 mitigation, part 1: review JSON whose `summary` contains
/// markdown-escape-attempt content (HTML-style `<script>` tags
/// or markdown that tries to look like a URL) is still syntax-
/// valid against our schema and posts cleanly to Forgejo. The
/// schema doesn't sanitize content (Forgejo handles markdown
/// rendering safely), but it MUST validate that the LLM can't
/// inject e.g. unknown top-level fields that change the wire
/// shape of the API call.
#[test]
fn t9_review_json_with_unexpected_fields_is_rejected() {
    let raw = r#"{
        "summary": "looks fine",
        "walkthrough": "",
        "mermaid": "",
        "findings": [],
        "auto_merge": true
    }"#;
    let err = validate_review_output(raw).expect_err("unknown field must be rejected by schema");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("auto_merge")
            || msg.to_lowercase().contains("unknown")
            || msg.to_lowercase().contains("field"),
        "error should reference the unknown field; got: {msg}",
    );
}

/// T9 mitigation, part 2: severity is a closed enum. The LLM
/// can't smuggle a `"severity": "auto_approve"` past the
/// validator and have it round-trip into the review request.
#[test]
fn t9_review_finding_with_unknown_severity_is_rejected() {
    let raw = r#"{
        "summary": "x",
        "walkthrough": "",
        "mermaid": "",
        "findings": [{
            "path": "a.rs",
            "line_start": 1,
            "severity": "auto_approve",
            "message": "ship it"
        }]
    }"#;
    assert!(validate_review_output(raw).is_err());
}

/// T9 mitigation, part 3: even when the LLM provides legitimate
/// review JSON, the API verb is constructed by `ar_forgejo`, not
/// the LLM. Advisory findings approve with comments; the only way
/// to get `RequestChanges` is for at least one finding to have
/// severity Error.
#[test]
fn t9_review_event_is_derived_from_severity_not_llm_input() {
    let only_notes = ar_prompts::ReviewOutput {
        summary: "minor".into(),
        walkthrough: String::new(),
        mermaid: String::new(),
        findings: vec![ar_prompts::ReviewFinding {
            path: "a.rs".into(),
            line_start: 1,
            line_end: None,
            severity: ReviewSeverity::Note,
            message: "style nit".into(),
        }],
    };
    let req = output_to_review_request(&only_notes, "abcdef");
    assert_eq!(req.event, ar_forgejo::ReviewEvent::Approved);

    let with_error = ar_prompts::ReviewOutput {
        findings: vec![ar_prompts::ReviewFinding {
            path: "a.rs".into(),
            line_start: 1,
            line_end: None,
            severity: ReviewSeverity::Error,
            message: "SQL injection".into(),
        }],
        ..only_notes
    };
    let req = output_to_review_request(&with_error, "abcdef");
    assert_eq!(req.event, ar_forgejo::ReviewEvent::RequestChanges);
}

/// T3 mitigation: the review schema doesn't carry any field that
/// can act as a "system override" — there's no
/// `bypass_review_for_this_pr`, no `trust_pr_body`, no escape
/// hatch the LLM could be tricked into setting via prompt
/// injection in the PR body. The schema's allow-list is the
/// load-bearing defence here; this test pins it.
#[test]
fn t3_review_schema_top_level_keys_are_an_allowlist() {
    let s = ar_prompts::review_schema();
    let props = s["properties"].as_object().expect("properties");
    let keys: std::collections::BTreeSet<&str> = props.keys().map(String::as_str).collect();
    // Exact set — adding a new top-level field must be a
    // conscious decision; this test pins it.
    let expected: std::collections::BTreeSet<&str> =
        ["summary", "walkthrough", "mermaid", "findings"]
            .into_iter()
            .collect();
    assert_eq!(keys, expected);
    // additionalProperties must be false so the LLM (or a
    // prompt-injecting attacker) can't sneak unknown fields past
    // the validator.
    assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
}

/// T3 mitigation, part 2: the verifier's output schema is also
/// an allow-list. A prompt-injecting attacker who tricks the
/// reasoning model into emitting an extra "approve" verdict
/// can't have it round-trip through verification.
#[test]
fn t3_verifier_schema_top_level_keys_are_an_allowlist() {
    let s = ar_prompts::verification_schema();
    let props = s["properties"].as_object().expect("properties");
    let keys: std::collections::BTreeSet<&str> = props.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> = ["verdicts"].into_iter().collect();
    assert_eq!(keys, expected);
    assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
}
