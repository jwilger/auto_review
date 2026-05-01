//! LLM-driven triage. Classifies each changed file via a cheap-tier
//! model so the pipeline can drop trivial/formatting/doc files from
//! the heavy reasoning prompt — same idea as CodeRabbit's two-tier
//! routing.
//!
//! Wiring into the orchestrator's run_review_job is the next step;
//! this module is the testable building block.

use crate::error::ReviewError;
use ar_forgejo::ChangedFile;
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router};
use ar_prompts::{
    triage_schema, triage_system_prompt, validate_triage_output, TriageClass, TriageOutput,
};

/// Run the cheap-tier model to classify each file in `files` based on
/// its diff snippet. Returns a [`TriageOutput`] aligned 1:1 with the
/// input. If the cheap tier isn't configured on the router, returns
/// `Ok(None)` so the caller can skip the filter.
///
/// `diff` is the unified PR diff; the prompt feeds it as a single
/// chunk (cheap models don't need surgical chunking — they're picking
/// a label, not generating the review).
pub async fn triage_files_with_llm(
    router: &Router,
    files: &[ChangedFile],
    diff: &str,
) -> Result<Option<TriageOutput>, ReviewError> {
    if router.provider(ModelTier::Cheap).is_err() {
        return Ok(None);
    }
    if files.is_empty() {
        return Ok(Some(TriageOutput {
            classifications: Vec::new(),
        }));
    }

    let prompt = render_user_prompt(files, diff);
    let req = CompleteRequest {
        system: Some(triage_system_prompt().to_string()),
        messages: vec![Message::user(prompt)],
        response_format: Some(ResponseFormat::JsonSchema {
            name: "Triage".to_string(),
            schema: triage_schema().clone(),
        }),
        ..Default::default()
    };

    let resp = router.complete(ModelTier::Cheap, req).await?;
    match validate_triage_output(&resp.content) {
        Ok(out) => Ok(Some(out)),
        Err(e) => {
            tracing::warn!(error = %e, "triage output failed validation; falling back to no-triage");
            Ok(None)
        }
    }
}

/// Filter `files` to only those classified as Simple or Complex by the
/// triage output. Files missing from the classifications list are kept
/// (fail-open: better to over-review than skip a file the cheap model
/// forgot about).
pub fn filter_reviewable(files: &[ChangedFile], triage: &TriageOutput) -> Vec<ChangedFile> {
    let class_by_path: std::collections::HashMap<&str, TriageClass> = triage
        .classifications
        .iter()
        .map(|e| (e.path.as_str(), e.class))
        .collect();
    files
        .iter()
        .filter(|f| match class_by_path.get(f.filename.as_str()) {
            Some(c) => c.merits_full_review(),
            None => true,
        })
        .cloned()
        .collect()
}

/// Cap on the diff bytes embedded in the triage prompt. The
/// cheap-tier model classifies files as trivial / simple / complex;
/// it doesn't need the full diff to do that — file paths plus
/// representative hunks are enough. 40 KiB stays comfortably under
/// any cheap-model context limit and avoids burning a triage call
/// on a payload the model would just refuse anyway.
const TRIAGE_DIFF_CAP: usize = 40 * 1024;

fn render_user_prompt(files: &[ChangedFile], diff: &str) -> String {
    let mut out = String::with_capacity(diff.len().min(TRIAGE_DIFF_CAP) + 256);
    out.push_str("Files changed in this PR:\n");
    for f in files {
        out.push_str("- ");
        out.push_str(&f.filename);
        out.push('\n');
    }
    out.push_str("\nUnified diff:\n```diff\n");
    if diff.len() <= TRIAGE_DIFF_CAP {
        out.push_str(diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
    } else {
        let mut cut = TRIAGE_DIFF_CAP;
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push_str(&diff[..cut]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("[diff truncated for triage]\n");
    }
    out.push_str("```\n\nClassify each file using the schema.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{CompleteResponse, Error as LlmError, LlmProvider};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct CannedProvider(Mutex<Option<String>>);

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            let content = self.0.lock().unwrap().take().unwrap_or_default();
            Ok(CompleteResponse {
                content,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn cf(name: &str) -> ChangedFile {
        ChangedFile {
            filename: name.into(),
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: None,
        }
    }

    #[tokio::test]
    async fn returns_none_when_cheap_tier_unconfigured() {
        let router = Router::new();
        let result = triage_files_with_llm(&router, &[cf("a.rs")], "diff")
            .await
            .expect("ok");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn returns_empty_classifications_when_files_empty() {
        let provider = Arc::new(CannedProvider(Mutex::new(Some("ignored".into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = triage_files_with_llm(&router, &[], "diff")
            .await
            .expect("ok");
        let out = result.expect("Some");
        assert!(out.classifications.is_empty());
    }

    #[tokio::test]
    async fn returns_validated_classifications_on_well_formed_response() {
        let json = r#"{
            "classifications": [
                {"path":"a.rs","class":"complex"},
                {"path":"b.md","class":"doc"}
            ]
        }"#;
        let provider = Arc::new(CannedProvider(Mutex::new(Some(json.to_string()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = triage_files_with_llm(&router, &[cf("a.rs"), cf("b.md")], "diff")
            .await
            .expect("ok");
        let out = result.expect("Some");
        assert_eq!(out.classifications.len(), 2);
        assert_eq!(out.classifications[0].class, TriageClass::Complex);
        assert_eq!(out.classifications[1].class, TriageClass::Doc);
    }

    #[tokio::test]
    async fn returns_none_on_malformed_response_for_safety() {
        let provider = Arc::new(CannedProvider(Mutex::new(Some("not json".into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = triage_files_with_llm(&router, &[cf("a.rs")], "diff")
            .await
            .expect("ok");
        // Malformed → fall back to None so the caller skips the filter
        // entirely rather than dropping files based on garbage.
        assert!(result.is_none());
    }

    #[test]
    fn filter_reviewable_keeps_simple_and_complex_drops_others() {
        let files = vec![cf("a.rs"), cf("b.md"), cf("c.py"), cf("d.lock")];
        let triage = TriageOutput {
            classifications: vec![
                ar_prompts::TriageEntry {
                    path: "a.rs".into(),
                    class: TriageClass::Complex,
                },
                ar_prompts::TriageEntry {
                    path: "b.md".into(),
                    class: TriageClass::Doc,
                },
                ar_prompts::TriageEntry {
                    path: "c.py".into(),
                    class: TriageClass::Simple,
                },
                ar_prompts::TriageEntry {
                    path: "d.lock".into(),
                    class: TriageClass::Trivial,
                },
            ],
        };
        let kept = filter_reviewable(&files, &triage);
        let names: Vec<&str> = kept.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "c.py"]);
    }

    #[test]
    fn render_user_prompt_passes_short_diff_through_unchanged() {
        let files = vec![cf("a.rs")];
        let diff = "diff --git a/a.rs b/a.rs\n+new line\n";
        let out = render_user_prompt(&files, diff);
        assert!(out.contains("+new line"));
        assert!(!out.contains("[diff truncated for triage]"));
    }

    #[test]
    fn render_user_prompt_caps_oversized_diff_with_marker() {
        // A multi-MB diff would otherwise burn a cheap-tier API
        // call on a payload the model would refuse for context
        // overflow. Cap to 40 KiB and surface the truncation.
        let files = vec![cf("a.rs")];
        let huge = "x".repeat(80_000);
        let out = render_user_prompt(&files, &huge);
        assert!(out.contains("[diff truncated for triage]"));
        // Total prompt should be bounded by the cap + framing.
        assert!(
            out.len() < 50_000,
            "expected capped prompt, got {} bytes",
            out.len()
        );
    }

    #[test]
    fn filter_reviewable_keeps_unclassified_files_fail_open() {
        let files = vec![cf("a.rs"), cf("unknown.go")];
        let triage = TriageOutput {
            classifications: vec![ar_prompts::TriageEntry {
                path: "a.rs".into(),
                class: TriageClass::Complex,
            }],
        };
        let kept = filter_reviewable(&files, &triage);
        let names: Vec<&str> = kept.iter().map(|f| f.filename.as_str()).collect();
        assert!(names.contains(&"a.rs"));
        assert!(names.contains(&"unknown.go"));
    }
}
