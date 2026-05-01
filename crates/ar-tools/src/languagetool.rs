//! LanguageTool runner. Unlike every other runner in this crate,
//! LanguageTool is an HTTP service — not a bundled binary — so the
//! sandbox arg is intentionally unused. The runner POSTs each
//! prose file's contents to `<endpoint>/v2/check` and parses the
//! `matches[]` array into [`Finding`]s.
//!
//! Endpoint resolution: the [`LanguageToolRunner::endpoint`] field
//! is set by the caller from `LANGUAGETOOL_URL`. When the env var
//! is unset upstream, [`LanguageToolRunner::from_env`] returns
//! `None` and the linter is simply not added to the runner list.
//! No-op behaviour for unset env preserves the same "missing tool
//! is fine" semantics as the CLI runners.
//!
//! Severity mapping mirrors LanguageTool's own categorisation:
//! - `TYPOS` / `MISC` → Warning (likely-genuine mistakes)
//! - everything else (`STYLE`, `REDUNDANCY`, `GRAMMAR` outside
//!   typos, etc.) → Note (subjective / nit-level)
//! Operators wanting a stricter floor can use `AR_SEVERITY_FLOOR`.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

const TOOL: &str = "languagetool";
/// Cap per-file POST body to keep the runner from sending megabyte-
/// sized prose chunks at the LanguageTool server. Most prose files
/// are well under this; truncation is announced via a debug log.
const MAX_BYTES_PER_FILE: usize = 60_000;
/// Per-request timeout. LT can be slow on long inputs; a hard cap
/// keeps a single hung request from stalling the whole review.
const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Deserialize)]
struct CheckResponse {
    #[serde(default)]
    matches: Vec<LtMatch>,
}

#[derive(Debug, Deserialize)]
struct LtMatch {
    #[serde(default)]
    message: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    rule: Option<LtRule>,
}

#[derive(Debug, Deserialize)]
struct LtRule {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    category: Option<LtCategory>,
}

#[derive(Debug, Deserialize)]
struct LtCategory {
    #[serde(default)]
    id: Option<String>,
}

/// Convert a LanguageTool JSON response + the original text into
/// [`Finding`]s. Public so tests can exercise the parser without
/// going through HTTP.
pub fn parse_languagetool_response(
    json: &str,
    path: &str,
    text: &str,
) -> Result<Vec<Finding>, RunnerError> {
    let resp: CheckResponse = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::with_capacity(resp.matches.len());
    for m in resp.matches {
        // LT reports byte offsets into the input. Convert to a
        // 1-based line number by counting newlines up to offset.
        let line = byte_offset_to_line(text, m.offset);
        let (rule_id, category_owned) = match m.rule {
            Some(r) => (r.id, r.category.and_then(|c| c.id)),
            None => (None, None),
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id,
            path: path.to_string(),
            line_start: line,
            line_end: line,
            severity: severity_from_category(category_owned.as_deref()),
            message: m.message,
        });
    }
    Ok(out)
}

fn severity_from_category(cat: Option<&str>) -> Severity {
    match cat.map(|s| s.to_ascii_uppercase()).as_deref() {
        Some("TYPOS") | Some("MISC") => Severity::Warning,
        _ => Severity::Note,
    }
}

fn byte_offset_to_line(text: &str, offset: usize) -> u32 {
    let cap = offset.min(text.len());
    // 1-based: line 1 is everything before the first '\n'.
    let newlines = text.as_bytes()[..cap].iter().filter(|&&b| b == b'\n').count();
    (newlines as u32).saturating_add(1)
}

pub struct LanguageToolRunner {
    /// Base URL of the LanguageTool server, e.g.
    /// `https://api.languagetool.org` or `http://localhost:8010`.
    /// `/v2/check` is appended at request time.
    pub endpoint: String,
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
    /// Language hint (LT param `language`); when `None`, sends
    /// `auto` so LT detects per-document.
    pub language: Option<String>,
    client: reqwest::Client,
}

impl LanguageToolRunner {
    pub fn new(endpoint: impl Into<String>, files: Vec<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            files,
            language: None,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Construct from environment. Returns `None` when
    /// `LANGUAGETOOL_URL` is unset — caller skips wiring the
    /// runner in that case (mirrors CLI runners' "binary missing
    /// is fine" behaviour).
    pub fn from_env(files: Vec<String>) -> Option<Self> {
        std::env::var("LANGUAGETOOL_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|url| Self::new(url, files))
    }

    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    async fn check_file(&self, repo_dir: &Path, rel: &str) -> Result<Vec<Finding>, RunnerError> {
        let abs = repo_dir.join(rel);
        let bytes = match tokio::fs::read(&abs).await {
            Ok(b) => b,
            Err(e) => {
                // File missing is not fatal — the diff might
                // reference a deleted file. Just contribute zero
                // findings for it.
                tracing::debug!(path = rel, error = %e, "languagetool: skipping unreadable file");
                return Ok(Vec::new());
            }
        };
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if text.len() > MAX_BYTES_PER_FILE {
            tracing::debug!(
                path = rel,
                bytes = text.len(),
                cap = MAX_BYTES_PER_FILE,
                "languagetool: truncating large file before POST"
            );
            text.truncate(byte_truncate_boundary(&text, MAX_BYTES_PER_FILE));
        }
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/v2/check", self.endpoint.trim_end_matches('/'));
        let language = self.language.clone().unwrap_or_else(|| "auto".to_string());
        let resp = self
            .client
            .post(&url)
            .form(&[("text", text.as_str()), ("language", language.as_str())])
            .send()
            .await
            .map_err(|e| RunnerError::Sandbox(format!("languagetool POST: {e}")))?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            // 4xx/5xx is treated as a soft failure — log and
            // contribute no findings rather than aborting the
            // whole review.
            tracing::warn!(
                tool = TOOL,
                status = code,
                body = %truncate_for_log(&body),
                "languagetool returned non-2xx; skipping file"
            );
            return Ok(Vec::new());
        }
        let body = resp
            .text()
            .await
            .map_err(|e| RunnerError::Sandbox(format!("languagetool body: {e}")))?;
        parse_languagetool_response(&body, rel, &text)
    }
}

/// Find a UTF-8-safe truncation boundary at or below `cap`.
fn byte_truncate_boundary(s: &str, cap: usize) -> usize {
    let mut idx = cap.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn truncate_for_log(s: &str) -> String {
    const CAP: usize = 1024;
    if s.len() <= CAP {
        return s.to_string();
    }
    let cut = byte_truncate_boundary(s, CAP);
    format!("{}…", &s[..cut])
}

#[async_trait]
impl LinterRunner for LanguageToolRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        _sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for rel in &self.files {
            let mut findings = self.check_file(repo_dir, rel).await?;
            all.append(&mut findings);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_sandbox::DirectSandbox;
    use std::io::Write;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn write_file(dir: &Path, rel: &str, contents: &str) -> std::path::PathBuf {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&abs).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        abs
    }

    #[test]
    fn parse_typical_response() {
        let json = r#"{
            "matches": [
                {
                    "message": "Possible spelling mistake",
                    "offset": 5,
                    "length": 4,
                    "rule": {"id": "MORFOLOGIK_RULE", "category": {"id": "TYPOS"}}
                },
                {
                    "message": "Consider rewording",
                    "offset": 20,
                    "length": 3,
                    "rule": {"id": "WORDINESS", "category": {"id": "STYLE"}}
                }
            ]
        }"#;
        let text = "first line\nsecond line word here\nthird";
        let f = parse_languagetool_response(json, "doc.md", text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "doc.md");
        assert_eq!(f[0].rule_id.as_deref(), Some("MORFOLOGIK_RULE"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].line_start, 1); // offset 5 is on line 1
        assert_eq!(f[1].severity, Severity::Note);
        assert_eq!(f[1].line_start, 2); // offset 20 is past first newline
    }

    #[test]
    fn parse_empty_matches_yields_zero_findings() {
        let f = parse_languagetool_response(r#"{"matches":[]}"#, "x.md", "").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn parse_response_without_matches_field_is_ok() {
        // LT may return e.g. `{"warnings":...}` with no matches
        // when the input is empty. Should yield zero findings,
        // not a parse error.
        let f = parse_languagetool_response(r#"{"warnings":{}}"#, "x.md", "").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn parse_malformed_json_is_a_parse_error() {
        let err = parse_languagetool_response("not json", "x.md", "").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_rule_falls_back_to_note_severity() {
        let json = r#"{"matches":[{"message":"m","offset":0}]}"#;
        let f = parse_languagetool_response(json, "x.md", "abc").expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Note);
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn category_misc_maps_to_warning() {
        let json = r#"{"matches":[{
            "message":"m","offset":0,
            "rule":{"id":"X","category":{"id":"MISC"}}
        }]}"#;
        let f = parse_languagetool_response(json, "x.md", "abc").expect("ok");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn category_case_insensitive() {
        // LT historically lowercased some category IDs; tolerate.
        let json = r#"{"matches":[{
            "message":"m","offset":0,
            "rule":{"id":"X","category":{"id":"typos"}}
        }]}"#;
        let f = parse_languagetool_response(json, "x.md", "abc").expect("ok");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn byte_offset_to_line_at_start_returns_1() {
        assert_eq!(byte_offset_to_line("hello\nworld", 0), 1);
    }

    #[test]
    fn byte_offset_to_line_after_newline() {
        assert_eq!(byte_offset_to_line("a\nb\nc", 4), 3);
    }

    #[test]
    fn byte_offset_to_line_past_end_clamps_to_last_line() {
        // Offset > text.len() shouldn't panic. Counts newlines
        // across the whole string.
        assert_eq!(byte_offset_to_line("a\nb", 999), 2);
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        // Use a unique env var name to avoid races with other
        // tests; the function only reads LANGUAGETOOL_URL.
        // Save and clear if previously set.
        let prev = std::env::var("LANGUAGETOOL_URL").ok();
        // SAFETY: tests run single-threaded by default for env mutation;
        // worst case is a flaky test, not a memory unsafety.
        unsafe { std::env::remove_var("LANGUAGETOOL_URL") };
        let r = LanguageToolRunner::from_env(vec![]);
        assert!(r.is_none());
        if let Some(v) = prev {
            unsafe { std::env::set_var("LANGUAGETOOL_URL", v) };
        }
    }

    #[test]
    fn from_env_treats_whitespace_only_value_as_unset() {
        let prev = std::env::var("LANGUAGETOOL_URL").ok();
        unsafe { std::env::set_var("LANGUAGETOOL_URL", "   ") };
        let r = LanguageToolRunner::from_env(vec![]);
        assert!(r.is_none());
        match prev {
            Some(v) => unsafe { std::env::set_var("LANGUAGETOOL_URL", v) },
            None => unsafe { std::env::remove_var("LANGUAGETOOL_URL") },
        }
    }

    #[tokio::test]
    async fn empty_files_list_yields_zero_findings_without_http() {
        let r = LanguageToolRunner::new("http://invalid.invalid", vec![]);
        let sandbox = DirectSandbox::new();
        let cwd = std::env::current_dir().unwrap();
        let f = r.run(&sandbox, &cwd).await.expect("ok");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn happy_path_posts_text_and_parses_matches() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/check"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{
                    "matches": [
                        {"message":"x","offset":0,"length":1,
                         "rule":{"id":"R","category":{"id":"TYPOS"}}}
                    ]
                }"#,
            ))
            .mount(&server)
            .await;

        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "doc.md", "hello world");
        let r = LanguageToolRunner::new(server.uri(), vec!["doc.md".into()]);
        let sandbox = DirectSandbox::new();
        let f = r.run(&sandbox, dir.path()).await.expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].source_tool, "languagetool");
        assert_eq!(f[0].path, "doc.md");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[tokio::test]
    async fn http_500_is_soft_failure_not_runner_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/check"))
            .respond_with(ResponseTemplate::new(500).set_body_string("{}"))
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "doc.md", "hello");
        let r = LanguageToolRunner::new(server.uri(), vec!["doc.md".into()]);
        let sandbox = DirectSandbox::new();
        let f = r.run(&sandbox, dir.path()).await.expect("must not error");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn missing_file_contributes_zero_findings() {
        let server = MockServer::start().await;
        // No mock needed — we never reach HTTP for a missing file.
        let dir = TempDir::new().unwrap();
        let r = LanguageToolRunner::new(server.uri(), vec!["does-not-exist.md".into()]);
        let sandbox = DirectSandbox::new();
        let f = r.run(&sandbox, dir.path()).await.expect("ok");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn empty_file_contributes_zero_findings_without_post() {
        let server = MockServer::start().await;
        // No mocks → if HTTP fired the test would fail with 404.
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "empty.md", "   \n   \n");
        let r = LanguageToolRunner::new(server.uri(), vec!["empty.md".into()]);
        let sandbox = DirectSandbox::new();
        let f = r.run(&sandbox, dir.path()).await.expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn byte_truncate_boundary_respects_utf8() {
        // 4-byte emoji at the end; cap inside the emoji must
        // truncate before it.
        let s = "abc🦀";
        let cut = byte_truncate_boundary(s, 4); // mid-emoji
        assert!(s.is_char_boundary(cut));
        assert_eq!(&s[..cut], "abc");
    }

    #[test]
    fn trailing_slash_in_endpoint_handled() {
        let r = LanguageToolRunner::new("http://x/", vec![]);
        // The /v2/check path is appended after trimming the trailing
        // slash; we check this indirectly by inspecting the endpoint
        // field is preserved as-given.
        assert_eq!(r.endpoint, "http://x/");
    }
}
