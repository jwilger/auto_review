//! Fixture-replay benchmark harness.
//!
//! Loads PR fixtures from disk, renders the same review prompt the
//! orchestrator would, and calls the LLM directly via the same
//! self-heal + verifier path the gateway uses. Skips Forgejo entirely
//! — fixtures carry their own diff and changed-files list.
//!
//! Useful for: picking a reasoning model, tuning prompt content,
//! tracking review-quality regressions over time, comparing local
//! Ollama models to cloud providers on a fixed corpus.

use crate::cli::BenchArgs;
use anyhow::{Context, Result};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_prompts::ReviewOutput;
use ar_prompts::{render_review_prompt, system_prompt, ReviewPromptInputs};
use ar_review::{generate_with_self_heal, verify_findings, HealConfig};
use ar_tools::Finding;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Deserialize, Default)]
pub struct Fixture {
    pub name: String,
    #[serde(default)]
    pub repo_full_name: String,
    #[serde(default)]
    pub pr_number: u64,
    pub pr_title: String,
    #[serde(default)]
    pub pr_body: String,
    pub diff: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub linter_findings: Vec<Finding>,
    #[serde(default)]
    pub guidelines: String,
    #[serde(default)]
    pub repo_context: String,
    /// Optional ground-truth findings the reviewer is expected to
    /// surface. When present, the bench harness computes
    /// precision/recall against them by matching on (path, line).
    #[serde(default)]
    pub expected: Vec<ExpectedFinding>,
}

/// One ground-truth finding for a labelled fixture. We match by
/// (path, line) — the LLM's exact wording will vary across runs and
/// models, so message-equality isn't useful for scoring. `note` is
/// for human readers (the JSON file) only; it doesn't influence
/// scoring and isn't read at runtime.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExpectedFinding {
    pub path: String,
    pub line: u32,
    #[serde(default)]
    #[allow(dead_code)]
    pub note: String,
}

#[derive(Debug, Serialize)]
struct FixtureResult {
    name: String,
    findings_initial: usize,
    findings_after_verify: usize,
    latency_ms: u128,
    error: Option<String>,
    /// Per-fixture precision/recall stats. `None` when the fixture
    /// has no `expected` array (regression-tracking only).
    #[serde(skip_serializing_if = "Option::is_none")]
    label_match: Option<LabelMatch>,
}

/// Confusion-matrix-style match against a fixture's labelled
/// `expected` findings. `kept` and `missed` partition the expected
/// set; `spurious` is the count of model findings that weren't in
/// the expected set.
#[derive(Debug, Clone, Serialize)]
struct LabelMatch {
    expected: usize,
    matched: usize,
    missed: usize,
    spurious: usize,
}

#[derive(Debug, Serialize)]
struct Aggregate {
    fixtures: usize,
    successes: usize,
    failures: usize,
    total_findings_initial: usize,
    total_findings_after_verify: usize,
    mean_latency_ms: u128,
    median_latency_ms: u128,
    p99_latency_ms: u128,
    /// Aggregate precision/recall over fixtures that carry
    /// `expected` labels. `None` when no fixture is labelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    label_score: Option<LabelScore>,
}

/// Aggregate precision/recall across all labelled fixtures in a
/// run. Computed only over fixtures whose `expected` array is
/// non-empty; unlabelled fixtures don't contribute.
#[derive(Debug, Clone, Serialize)]
struct LabelScore {
    labelled_fixtures: usize,
    expected_total: usize,
    matched_total: usize,
    missed_total: usize,
    spurious_total: usize,
    /// matched / (matched + spurious) — fraction of model findings
    /// that lined up with a labelled-expected one.
    precision: f64,
    /// matched / (matched + missed) — fraction of labelled-expected
    /// findings the model actually surfaced.
    recall: f64,
}

pub async fn run(args: BenchArgs) -> Result<()> {
    let fixture_paths = expand_fixture_paths(&args.fixtures)?;
    if fixture_paths.is_empty() {
        anyhow::bail!("no fixtures matched the supplied paths");
    }

    let llm = build_router(&args)?;
    let verifier_enabled = llm.provider(ModelTier::Cheap).is_ok();

    let mut results: Vec<FixtureResult> = Vec::with_capacity(fixture_paths.len());
    for path in &fixture_paths {
        let result = run_one(path, &llm, verifier_enabled).await;
        if !args.json {
            print_row(&result);
        }
        results.push(result);
    }

    let aggregate = aggregate(&results);
    if args.json {
        println!("{}", serde_json::to_string(&aggregate)?);
    } else {
        print_aggregate(&aggregate, verifier_enabled);
    }
    Ok(())
}

/// Pull every `*.json` file out of any directory entries in `paths`,
/// keep file entries as-is, and return the deduplicated sorted list.
fn expand_fixture_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let meta = std::fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(p).with_context(|| format!("readdir {}", p.display()))? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.extension().and_then(|s| s.to_str()) == Some("json") {
                    out.push(entry_path);
                }
            }
        } else {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn build_router(args: &BenchArgs) -> Result<LlmRouter> {
    let reasoning = Arc::new(
        OpenAiProvider::new(
            &args.llm_base_url,
            args.llm_api_key.as_deref(),
            &args.llm_model,
        )
        .context("build reasoning provider")?,
    );
    let mut router = LlmRouter::new().with(ModelTier::Reasoning, reasoning);
    if let Some(cheap_model) = &args.llm_cheap_model {
        let cheap = Arc::new(
            OpenAiProvider::new(&args.llm_base_url, args.llm_api_key.as_deref(), cheap_model)
                .context("build cheap provider")?,
        );
        router = router.with(ModelTier::Cheap, cheap);
    }
    Ok(router)
}

async fn run_one(path: &Path, llm: &LlmRouter, verifier_enabled: bool) -> FixtureResult {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return FixtureResult {
                name: path.display().to_string(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some(format!("read fixture: {e}")),
                label_match: None,
            };
        }
    };
    let fixture: Fixture = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            return FixtureResult {
                name: path.display().to_string(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some(format!("parse fixture: {e}")),
                label_match: None,
            };
        }
    };

    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: &fixture.repo_full_name,
        pr_number: fixture.pr_number,
        pr_title: &fixture.pr_title,
        pr_body: &fixture.pr_body,
        diff: &fixture.diff,
        changed_files: &fixture.changed_files,
        linter_findings: &fixture.linter_findings,
        guidelines: &fixture.guidelines,
        repo_context: &fixture.repo_context,
    });

    let started = Instant::now();
    let initial =
        match generate_with_self_heal(llm, system_prompt(), &prompt, HealConfig::default()).await {
            Ok(o) => o,
            Err(e) => {
                return FixtureResult {
                    name: fixture.name,
                    findings_initial: 0,
                    findings_after_verify: 0,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(format!("review LLM call failed: {e}")),
                    label_match: None,
                };
            }
        };
    let findings_initial = initial.findings.len();

    let scored_output = if verifier_enabled {
        match verify_findings(llm, initial, &fixture.diff).await {
            Ok(o) => o,
            Err(e) => {
                return FixtureResult {
                    name: fixture.name,
                    findings_initial,
                    findings_after_verify: findings_initial,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(format!("verifier failed: {e}")),
                    label_match: None,
                };
            }
        }
    } else {
        initial
    };
    let after_verify = scored_output.findings.len();

    let label_match = if fixture.expected.is_empty() {
        None
    } else {
        Some(score_against_labels(&scored_output, &fixture.expected))
    };

    FixtureResult {
        name: fixture.name,
        findings_initial,
        findings_after_verify: after_verify,
        latency_ms: started.elapsed().as_millis(),
        error: None,
        label_match,
    }
}

/// Match the model's findings against the labelled `expected` set
/// by (path, line) coordinates. A model finding is "matched" if it
/// shares a (path, line) with some expected entry that hasn't been
/// claimed yet; otherwise it's "spurious". Expected entries that
/// nothing claimed are "missed".
fn score_against_labels(output: &ReviewOutput, expected: &[ExpectedFinding]) -> LabelMatch {
    let mut claimed: Vec<bool> = vec![false; expected.len()];
    let mut matched = 0usize;
    let mut spurious = 0usize;

    for f in &output.findings {
        let mut hit = false;
        for (i, e) in expected.iter().enumerate() {
            if claimed[i] {
                continue;
            }
            if e.path == f.path && e.line == f.line_start {
                claimed[i] = true;
                matched += 1;
                hit = true;
                break;
            }
        }
        if !hit {
            spurious += 1;
        }
    }

    let missed = claimed.iter().filter(|c| !**c).count();
    LabelMatch {
        expected: expected.len(),
        matched,
        missed,
        spurious,
    }
}

fn aggregate(results: &[FixtureResult]) -> Aggregate {
    let successes = results.iter().filter(|r| r.error.is_none()).count();
    let failures = results.len() - successes;
    let total_initial: usize = results.iter().map(|r| r.findings_initial).sum();
    let total_verified: usize = results.iter().map(|r| r.findings_after_verify).sum();

    let mut latencies: Vec<u128> = results
        .iter()
        .filter(|r| r.error.is_none())
        .map(|r| r.latency_ms)
        .collect();
    latencies.sort_unstable();
    let mean = if latencies.is_empty() {
        0
    } else {
        latencies.iter().sum::<u128>() / latencies.len() as u128
    };
    let median = percentile(&latencies, 50.0);
    let p99 = percentile(&latencies, 99.0);

    let label_score = compute_label_score(results);

    Aggregate {
        fixtures: results.len(),
        successes,
        failures,
        total_findings_initial: total_initial,
        total_findings_after_verify: total_verified,
        mean_latency_ms: mean,
        median_latency_ms: median,
        p99_latency_ms: p99,
        label_score,
    }
}

fn compute_label_score(results: &[FixtureResult]) -> Option<LabelScore> {
    let labelled: Vec<&LabelMatch> = results
        .iter()
        .filter_map(|r| r.label_match.as_ref())
        .collect();
    if labelled.is_empty() {
        return None;
    }
    let expected_total: usize = labelled.iter().map(|m| m.expected).sum();
    let matched_total: usize = labelled.iter().map(|m| m.matched).sum();
    let missed_total: usize = labelled.iter().map(|m| m.missed).sum();
    let spurious_total: usize = labelled.iter().map(|m| m.spurious).sum();
    // Precision = TP / (TP + FP). FP = spurious.
    let precision = if matched_total + spurious_total == 0 {
        0.0
    } else {
        matched_total as f64 / (matched_total + spurious_total) as f64
    };
    // Recall = TP / (TP + FN). FN = missed.
    let recall = if matched_total + missed_total == 0 {
        0.0
    } else {
        matched_total as f64 / (matched_total + missed_total) as f64
    };
    Some(LabelScore {
        labelled_fixtures: labelled.len(),
        expected_total,
        matched_total,
        missed_total,
        spurious_total,
        precision,
        recall,
    })
}

/// Nearest-rank percentile. `pct` is in `[0, 100]`. Returns 0 on
/// empty input.
fn percentile(sorted: &[u128], pct: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let pct = pct.clamp(0.0, 100.0);
    let n = sorted.len() as f64;
    let rank = (pct / 100.0 * n).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn print_row(r: &FixtureResult) {
    if let Some(err) = &r.error {
        println!("{:>40}  ERROR  {err}", r.name);
        return;
    }
    let base = format!(
        "{:>40}  {:>3} findings  → {:>3} after verify  {:>6} ms",
        r.name, r.findings_initial, r.findings_after_verify, r.latency_ms,
    );
    if let Some(lm) = &r.label_match {
        println!(
            "{base}   labels: {}/{} matched, {} missed, {} spurious",
            lm.matched, lm.expected, lm.missed, lm.spurious
        );
    } else {
        println!("{base}");
    }
}

fn print_aggregate(a: &Aggregate, verifier_enabled: bool) {
    println!();
    println!("─── Aggregate ───");
    println!("  fixtures:                    {}", a.fixtures);
    println!("  successes:                   {}", a.successes);
    println!("  failures:                    {}", a.failures);
    println!(
        "  total findings (initial):    {}",
        a.total_findings_initial
    );
    if verifier_enabled {
        println!(
            "  total findings (verified):   {}",
            a.total_findings_after_verify
        );
        let dropped = a
            .total_findings_initial
            .saturating_sub(a.total_findings_after_verify);
        println!("  dropped by verifier:         {dropped}");
    }
    println!("  mean latency:                {} ms", a.mean_latency_ms);
    println!("  median latency:              {} ms", a.median_latency_ms);
    println!("  p99 latency:                 {} ms", a.p99_latency_ms);
    if let Some(s) = &a.label_score {
        println!();
        println!(
            "─── Label scoring ({} labelled fixture(s)) ───",
            s.labelled_fixtures
        );
        println!("  expected total:              {}", s.expected_total);
        println!("  matched:                     {}", s.matched_total);
        println!("  missed:                      {}", s.missed_total);
        println!("  spurious:                    {}", s.spurious_total);
        println!("  precision:                   {:.3}", s.precision);
        println!("  recall:                      {:.3}", s.recall);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn percentile_handles_empty_input() {
        assert_eq!(percentile(&[], 50.0), 0);
        assert_eq!(percentile(&[], 99.0), 0);
    }

    #[test]
    fn percentile_returns_max_at_one_hundred() {
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 100.0), 5);
    }

    #[test]
    fn percentile_returns_min_at_zero() {
        // pct=0 ⇒ rank=0 ⇒ idx=0 (clamped via saturating_sub)
        assert_eq!(percentile(&[7, 9, 11], 0.0), 7);
    }

    #[test]
    fn percentile_p50_is_median() {
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50.0), 3);
    }

    #[test]
    fn aggregate_counts_successes_and_failures() {
        let results = vec![
            FixtureResult {
                name: "a".into(),
                findings_initial: 3,
                findings_after_verify: 2,
                latency_ms: 100,
                error: None,
                label_match: None,
            },
            FixtureResult {
                name: "b".into(),
                findings_initial: 1,
                findings_after_verify: 1,
                latency_ms: 200,
                error: None,
                label_match: None,
            },
            FixtureResult {
                name: "c".into(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some("boom".into()),
                label_match: None,
            },
        ];
        let agg = aggregate(&results);
        assert_eq!(agg.fixtures, 3);
        assert_eq!(agg.successes, 2);
        assert_eq!(agg.failures, 1);
        assert_eq!(agg.total_findings_initial, 4);
        assert_eq!(agg.total_findings_after_verify, 3);
        assert_eq!(agg.mean_latency_ms, 150);
        assert_eq!(agg.median_latency_ms, 100);
        assert!(
            agg.label_score.is_none(),
            "no labelled fixtures = no label score"
        );
    }

    #[test]
    fn label_match_partitions_findings_correctly() {
        use ar_prompts::{ReviewFinding, ReviewSeverity};
        let output = ReviewOutput {
            summary: String::new(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: vec![
                ReviewFinding {
                    path: "src/auth.rs".into(),
                    line_start: 42,
                    line_end: None,
                    severity: ReviewSeverity::Warning,
                    message: "x".into(),
                },
                ReviewFinding {
                    path: "src/util.rs".into(),
                    line_start: 7,
                    line_end: None,
                    severity: ReviewSeverity::Note,
                    message: "y".into(),
                },
            ],
        };
        let expected = vec![
            ExpectedFinding {
                path: "src/auth.rs".into(),
                line: 42,
                note: "should be flagged".into(),
            },
            ExpectedFinding {
                path: "src/missed.rs".into(),
                line: 1,
                note: "model didn't see this".into(),
            },
        ];
        let m = score_against_labels(&output, &expected);
        // First model finding matches first expected. Second model
        // finding is spurious. Second expected is missed.
        assert_eq!(m.expected, 2);
        assert_eq!(m.matched, 1);
        assert_eq!(m.missed, 1);
        assert_eq!(m.spurious, 1);
    }

    #[test]
    fn label_match_each_expected_consumed_at_most_once() {
        use ar_prompts::{ReviewFinding, ReviewSeverity};
        // Two model findings at the same (path, line); only one
        // expected to share that coordinate. The second model
        // finding can't claim the already-consumed expected.
        let output = ReviewOutput {
            summary: String::new(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: vec![
                ReviewFinding {
                    path: "x.rs".into(),
                    line_start: 1,
                    line_end: None,
                    severity: ReviewSeverity::Warning,
                    message: "a".into(),
                },
                ReviewFinding {
                    path: "x.rs".into(),
                    line_start: 1,
                    line_end: None,
                    severity: ReviewSeverity::Warning,
                    message: "b".into(),
                },
            ],
        };
        let expected = vec![ExpectedFinding {
            path: "x.rs".into(),
            line: 1,
            note: String::new(),
        }];
        let m = score_against_labels(&output, &expected);
        assert_eq!(m.matched, 1);
        assert_eq!(m.spurious, 1);
        assert_eq!(m.missed, 0);
    }

    #[test]
    fn label_score_aggregates_precision_and_recall() {
        let results = vec![
            FixtureResult {
                name: "a".into(),
                findings_initial: 3,
                findings_after_verify: 3,
                latency_ms: 10,
                error: None,
                label_match: Some(LabelMatch {
                    expected: 3,
                    matched: 2,
                    missed: 1,
                    spurious: 1,
                }),
            },
            FixtureResult {
                name: "b".into(),
                findings_initial: 2,
                findings_after_verify: 2,
                latency_ms: 20,
                error: None,
                label_match: Some(LabelMatch {
                    expected: 2,
                    matched: 2,
                    missed: 0,
                    spurious: 0,
                }),
            },
        ];
        let agg = aggregate(&results);
        let score = agg.label_score.expect("score present");
        assert_eq!(score.labelled_fixtures, 2);
        assert_eq!(score.matched_total, 4);
        assert_eq!(score.missed_total, 1);
        assert_eq!(score.spurious_total, 1);
        // precision = 4 / (4 + 1) = 0.8
        assert!((score.precision - 0.8).abs() < 1e-9);
        // recall    = 4 / (4 + 1) = 0.8
        assert!((score.recall - 0.8).abs() < 1e-9);
    }

    #[test]
    fn expand_fixture_paths_handles_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.json");
        let f2 = dir.path().join("b.json");
        let other = dir.path().join("c.txt");
        for p in [&f1, &f2, &other] {
            let mut fh = std::fs::File::create(p).unwrap();
            fh.write_all(b"{}").unwrap();
        }
        let extra = dir.path().join("explicit.json");
        std::fs::File::create(&extra)
            .unwrap()
            .write_all(b"{}")
            .unwrap();

        let resolved = expand_fixture_paths(&[dir.path().into(), extra.clone()]).unwrap();

        // dedup: explicit.json was added twice (via dir + explicit) but
        // shouldn't appear twice in the result. The .txt file is dropped.
        let names: Vec<&str> = resolved
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"a.json"));
        assert!(names.contains(&"b.json"));
        assert!(names.contains(&"explicit.json"));
        assert!(!names.contains(&"c.txt"));
        assert_eq!(
            names.iter().filter(|n| **n == "explicit.json").count(),
            1,
            "explicit.json appeared twice"
        );
    }

    #[test]
    fn expand_fixture_paths_errors_on_nonexistent_path() {
        let result = expand_fixture_paths(&[PathBuf::from("/no/such/path/xyz")]);
        assert!(result.is_err());
    }

    /// The shipped `bench/fixtures/*.json` files double as documentation
    /// of the fixture format. If the `Fixture` struct shape changes in a
    /// way that breaks them, the bench harness silently rejects every
    /// fixture in the wild as "parse fixture" errors. This test catches
    /// that at PR time instead of at-deploy time.
    #[test]
    fn shipped_fixtures_all_parse() {
        let workspace_root = std::env::var("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .ok()
            .and_then(|p| {
                // CARGO_MANIFEST_DIR is .../crates/ar-cli; the bench
                // directory lives at the workspace root.
                p.parent()?.parent().map(Path::to_path_buf)
            });
        let Some(root) = workspace_root else {
            // Should never happen in cargo's test runner, but skip
            // gracefully if the env var is missing rather than failing
            // mysteriously.
            return;
        };
        let fixtures_dir = root.join("bench/fixtures");
        if !fixtures_dir.is_dir() {
            return; // No fixtures shipped — nothing to verify.
        }

        let entries = std::fs::read_dir(&fixtures_dir).expect("read bench/fixtures/");
        let mut count = 0usize;
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let _: Fixture = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
            count += 1;
        }
        assert!(count > 0, "no fixtures matched bench/fixtures/*.json");
    }
}
