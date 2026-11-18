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
use alloc::sync::Arc;
use anyhow::{Context as _, Result};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_prompts::ReviewOutput;
use ar_prompts::{
    fence_nonce, process_nonce_salt, render_review_prompt, system_prompt, ReviewPromptInputs,
};
use ar_review::{generate_with_self_heal, verify_findings, HealConfig};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Run-level summary of a bench run; also the `--json` output and the
/// `--baseline` input format.
#[derive(Debug, Serialize, Deserialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the key order of the `bench run --json` aggregate that operators save as baselines and diff"
)]
struct Aggregate {
    /// Number of fixtures the run loaded.
    fixtures: usize,
    /// Fixtures that completed without an error.
    successes: usize,
    /// Fixtures that ended in an error.
    failures: usize,
    /// Findings the review step produced, summed over fixtures.
    total_findings_initial: usize,
    /// Findings left after the verifier, summed over fixtures.
    total_findings_after_verify: usize,
    /// Mean latency of successful fixtures, in milliseconds.
    mean_latency_ms: u128,
    /// Median latency of successful fixtures, in milliseconds.
    median_latency_ms: u128,
    /// 99th-percentile latency of successful fixtures, in milliseconds.
    p99_latency_ms: u128,
    /// Aggregate precision/recall over fixtures that carry
    /// `expected` labels. `None` when no fixture is labelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    label_score: Option<LabelScore>,
}

/// Per-metric delta between a baseline and the current run.
/// `None` is used where one side has data and the other doesn't
/// (e.g. baseline had no labelled fixtures so no precision to
/// compare against).
#[derive(Debug, Clone, Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a baseline-to-current delta of the same-named Aggregate metric; the suffix keeps them distinct from the absolute values"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "fields follow the order print_comparison reports them in, which keeps the struct and its rendered table aligned"
)]
struct Comparison {
    /// Change in the fraction of fixtures that succeeded.
    success_rate_delta: Option<f64>,
    /// Change in labelled precision.
    precision_delta: Option<f64>,
    /// Change in labelled recall.
    recall_delta: Option<f64>,
    /// Change in mean latency, in milliseconds.
    mean_latency_ms_delta: i128,
    /// Change in p99 latency, in milliseconds.
    p99_latency_ms_delta: i128,
    /// Change in the number of findings left after verification.
    total_findings_delta: i64,
}

/// One ground-truth finding for a labelled fixture. We match by
/// (path, line) — the LLM's exact wording will vary across runs and
/// models, so message-equality isn't useful for scoring. `note` is
/// for human readers (the JSON file) only; it doesn't influence
/// scoring and isn't read at runtime.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExpectedFinding {
    /// Line the finding should anchor to.
    pub line: u32,
    /// Human-readable explanation of the expected finding.
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "labelled fixtures carry a note for human readers; the field documents that schema and scoring deliberately never reads it"
    )]
    pub note: String,
    /// Repository-relative path the finding should anchor to.
    pub path: String,
}

/// A recorded PR the bench replays: diff, metadata and optional
/// ground-truth labels.
#[derive(Debug, Deserialize, Default)]
pub struct Fixture {
    /// Changed file paths, as the orchestrator would list them.
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Unified diff of the PR.
    pub diff: String,
    /// Optional ground-truth findings the reviewer is expected to
    /// surface. When present, the bench harness computes
    /// precision/recall against them by matching on (path, line).
    #[serde(default)]
    pub expected: Vec<ExpectedFinding>,
    /// Repository review guidelines rendered into the prompt.
    #[serde(default)]
    pub guidelines: String,
    /// Fixture name shown in reports; also seeds the prompt fence nonce.
    pub name: String,
    /// PR description.
    #[serde(default)]
    pub pr_body: String,
    /// PR number rendered into the prompt.
    #[serde(default)]
    pub pr_number: u64,
    /// PR title.
    pub pr_title: String,
    /// Retrieved repository context rendered into the prompt.
    #[serde(default)]
    pub repo_context: String,
    /// `owner/repo` the PR belongs to.
    #[serde(default)]
    pub repo_full_name: String,
}

/// Outcome of replaying one fixture.
#[derive(Debug, Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "fields keep the order of the per-fixture report row: name, finding counts, latency, then error and label outcome"
)]
struct FixtureResult {
    /// Fixture name, or its path when the fixture could not be loaded.
    name: String,
    /// Findings the review step produced.
    findings_initial: usize,
    /// Findings left after the verifier.
    findings_after_verify: usize,
    /// Wall-clock time of the LLM calls, in milliseconds.
    latency_ms: u128,
    /// Why the fixture failed, when it did.
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
    /// Size of the labelled expected set.
    expected: usize,
    /// Expected findings the model reported.
    matched: usize,
    /// Expected findings the model did not report.
    missed: usize,
    /// Model findings with no expected counterpart.
    spurious: usize,
}

/// Aggregate precision/recall across all labelled fixtures in a
/// run. Computed only over fixtures whose `expected` array is
/// non-empty; unlabelled fixtures don't contribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the key order of label_score in the `bench run --json` aggregate that operators save as baselines and diff"
)]
struct LabelScore {
    /// Fixtures that carry labels.
    labelled_fixtures: usize,
    /// Expected findings summed over labelled fixtures.
    expected_total: usize,
    /// Matched findings summed over labelled fixtures.
    matched_total: usize,
    /// Missed findings summed over labelled fixtures.
    missed_total: usize,
    /// Spurious findings summed over labelled fixtures.
    spurious_total: usize,
    /// matched / (matched + spurious) — fraction of model findings
    /// that lined up with a labelled-expected one.
    precision: f64,
    /// matched / (matched + missed) — fraction of labelled-expected
    /// findings the model actually surfaced.
    recall: f64,
}

impl Comparison {
    /// Regression heuristic: `precision/recall/success_rate` drop by
    /// more than 5 percentage points (delta < -0.05), or p99 latency
    /// jumps by more than 5 seconds. Tunable, but these defaults
    /// are the ones operators want out of the box.
    fn is_regression(&self) -> bool {
        /// A drop of more than 5 percentage points.
        const MAX_DROP: f64 = -0.05;
        // Success-rate drop is the bluntest regression signal: a
        // model change that breaks JSON-schema output frequently
        // would tank this even when precision/recall stayed flat
        // for the surviving runs. Catch it explicitly rather than
        // hoping precision/recall would surface it.
        let dropped = |delta: Option<f64>| delta.is_some_and(|value| value < MAX_DROP);
        dropped(self.success_rate_delta)
            || dropped(self.precision_delta)
            || dropped(self.recall_delta)
            // The "50% jump in p99" check needs the baseline's p99 to
            // compute the percent; signal it via the absolute jump
            // instead. A 5-second p99 increase is alert-worthy regardless.
            || self.p99_latency_ms_delta > 5000
    }
}

/// `bench run`: replay every fixture, print per-fixture rows and the
/// aggregate (or the JSON aggregate), and compare against a baseline.
pub async fn run(args: BenchArgs) -> Result<()> {
    let fixture_paths = expand_fixture_paths(&args.fixtures)?;
    if fixture_paths.is_empty() {
        anyhow::bail!("no fixtures matched the supplied paths");
    }
    // clap's `env = ...` accepts empty assignments, overriding the
    // default_value silently. Without this check, `LLM_REASONING_MODEL=`
    // would build an OpenAiProvider with an empty model name and 400
    // every fixture; operators would chase the cryptic provider error
    // through dozens of failed runs before noticing the env. Bail
    // up-front with a fix-pointing message.
    if args.llm_base_url.trim().is_empty() {
        anyhow::bail!(
            "--llm-base-url (LLM_BASE_URL) is empty; set it to the API root \
             (e.g. https://api.openai.com or http://localhost:11434)"
        );
    }
    if args.llm_model.trim().is_empty() {
        anyhow::bail!(
            "--llm-model (LLM_REASONING_MODEL) is empty; set a real model \
             name or unset to use the default (qwen2.5-coder:32b)"
        );
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
        print_json_aggregate(&aggregate)?;
    } else {
        print_aggregate(&aggregate, verifier_enabled);
    }

    if let Some(baseline_path) = args.baseline.as_deref() {
        let baseline_body = fs::read_to_string(baseline_path)
            .with_context(|| format!("read baseline {}", baseline_path.display()))?;
        let baseline: Aggregate = serde_json::from_str(&baseline_body)
            .with_context(|| format!("parse baseline {}", baseline_path.display()))?;
        let comparison = compare(&baseline, &aggregate);
        print_comparison(&comparison, baseline_path);
        if args.fail_on_regression && comparison.is_regression() {
            anyhow::bail!(
                "regression vs baseline {}: see comparison above",
                baseline_path.display()
            );
        }
    }
    Ok(())
}

/// Print the aggregate as one JSON line on stdout for `--json`.
#[expect(
    clippy::print_stdout,
    reason = "`bench run --json` exists to print the aggregate to stdout for piping and baselines"
)]
fn print_json_aggregate(aggregate: &Aggregate) -> Result<()> {
    println!("{}", serde_json::to_string(aggregate)?);
    Ok(())
}

/// Per-metric differences `current - baseline`.
#[expect(
    clippy::float_arithmetic,
    reason = "success-rate, precision and recall deltas are differences of f64 ratios"
)]
fn compare(baseline: &Aggregate, current: &Aggregate) -> Comparison {
    let success_rate_delta = match (success_rate(baseline), success_rate(current)) {
        (Some(baseline_rate), Some(current_rate)) => Some(current_rate - baseline_rate),
        _ => None,
    };
    let precision_delta = match (baseline.label_score.as_ref(), current.label_score.as_ref()) {
        (Some(baseline_score), Some(current_score)) => {
            Some(current_score.precision - baseline_score.precision)
        }
        _ => None,
    };
    let recall_delta = match (baseline.label_score.as_ref(), current.label_score.as_ref()) {
        (Some(baseline_score), Some(current_score)) => {
            Some(current_score.recall - baseline_score.recall)
        }
        _ => None,
    };
    Comparison {
        success_rate_delta,
        precision_delta,
        recall_delta,
        mean_latency_ms_delta: latency_delta(baseline.mean_latency_ms, current.mean_latency_ms),
        p99_latency_ms_delta: latency_delta(baseline.p99_latency_ms, current.p99_latency_ms),
        total_findings_delta: count_delta(
            baseline.total_findings_after_verify,
            current.total_findings_after_verify,
        ),
    }
}

/// Fraction of fixtures that completed without error; `None` for an
/// empty run.
fn success_rate(agg: &Aggregate) -> Option<f64> {
    (agg.fixtures > 0).then(|| ratio(agg.successes, agg.fixtures))
}

/// `numerator / denominator` as an f64, or zero when the denominator is
/// zero.
#[expect(
    clippy::float_arithmetic,
    reason = "bench rates (success rate, precision, recall) are real-valued ratios of counts"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "the counts are fixture and finding tallies, far below the 2^52 limit where f64 stops representing integers exactly"
)]
#[expect(
    clippy::as_conversions,
    reason = "usize has no lossless conversion to f64; the cast is exact for these small tallies"
)]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// `current - baseline` for two latencies, saturating at the `i128`
/// range.
fn latency_delta(baseline: u128, current: u128) -> i128 {
    let signed = |millis: u128| i128::try_from(millis).unwrap_or(i128::MAX);
    signed(current).saturating_sub(signed(baseline))
}

/// `current - baseline` for two counts, saturating at the `i64` range.
fn count_delta(baseline: usize, current: usize) -> i64 {
    let signed = |count: usize| i64::try_from(count).unwrap_or(i64::MAX);
    signed(current).saturating_sub(signed(baseline))
}

/// Print the baseline comparison table.
#[expect(
    clippy::print_stdout,
    reason = "`bench run --baseline` reports the comparison table on stdout"
)]
fn print_comparison(comparison: &Comparison, baseline_path: &Path) {
    println!();
    println!("Comparison vs baseline ({})", baseline_path.display());
    println!("{}", "-".repeat(68));
    if let Some(delta) = comparison.success_rate_delta {
        println!("  success rate     {}", fmt_pct_delta(delta));
    }
    if let Some(delta) = comparison.precision_delta {
        println!("  precision        {}", fmt_pct_delta(delta));
    } else {
        println!("  precision        \u{2014} (one side unlabelled)");
    }
    if let Some(delta) = comparison.recall_delta {
        println!("  recall           {}", fmt_pct_delta(delta));
    } else {
        println!("  recall           \u{2014} (one side unlabelled)");
    }
    println!(
        "  mean latency     {}",
        fmt_ms_delta(comparison.mean_latency_ms_delta)
    );
    println!(
        "  p99 latency      {}",
        fmt_ms_delta(comparison.p99_latency_ms_delta)
    );
    println!(
        "  total findings   {}",
        fmt_int_delta(comparison.total_findings_delta)
    );
    if comparison.is_regression() {
        println!();
        println!(
            "  \u{26a0} REGRESSION: precision/recall dropped > 5pp, or p99 latency jumped > 5s."
        );
    }
}

/// Render a fractional delta as signed percentage points.
#[expect(
    clippy::float_arithmetic,
    reason = "scaling a fractional delta to percentage points is float math"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "the zero literal is compared against an f64 delta; a type suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
)]
fn fmt_pct_delta(delta: f64) -> String {
    let sign = if delta >= 0.0 { "+" } else { "" };
    format!("{sign}{:.2}pp", delta * 100.0)
}

/// Render a millisecond delta with an explicit sign.
fn fmt_ms_delta(delta: i128) -> String {
    let sign = if delta >= 0 { "+" } else { "" };
    format!("{sign}{delta}ms")
}

/// Render a count delta with an explicit sign.
fn fmt_int_delta(delta: i64) -> String {
    let sign = if delta >= 0 { "+" } else { "" };
    format!("{sign}{delta}")
}

/// Pull every `*.json` file out of any directory entries in `paths`
/// (recursively, so users can organise fixtures into subdirectories
/// like `fixtures/sqli/`, `fixtures/xss/`), keep file entries as-is,
/// and return the deduplicated sorted list.
fn expand_fixture_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            collect_json_recursive(path, &mut out)?;
        } else {
            out.push(path.clone());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Append every `*.json` regular file under `dir` to `out`.
fn collect_json_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for dir_entry in fs::read_dir(dir).with_context(|| format!("readdir {}", dir.display()))? {
        let entry = dir_entry?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("file_type {}", path.display()))?;
        if file_type.is_dir() {
            collect_json_recursive(&path, out)?;
        } else if is_regular_json_file(file_type, &path) {
            out.push(path);
        } else {
            // Skip symlinks and other types — fixture trees shouldn't
            // need them, and traversing symlinks risks infinite loops.
        }
    }
    Ok(())
}

/// Whether a directory entry is a regular file with a `.json` extension.
#[expect(
    clippy::filetype_is_file,
    reason = "only regular files are fixtures; symlinks and special files are skipped on purpose, so is_file's narrow meaning is the point"
)]
fn is_regular_json_file(file_type: fs::FileType, path: &Path) -> bool {
    file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
}

/// Router with the reasoning tier and, when configured, the cheap
/// verifier tier.
fn build_router(args: &BenchArgs) -> Result<LlmRouter> {
    let reasoning = Arc::new(
        OpenAiProvider::new(
            &args.llm_base_url,
            args.llm_api_key.as_deref(),
            &args.llm_model,
        )
        .context("build reasoning provider")?
        .with_omit_temperature(args.llm_omit_temperature),
    );
    let mut router = LlmRouter::new().with(ModelTier::Reasoning, reasoning);
    if let Some(cheap_model) = args.llm_cheap_model.as_deref() {
        let cheap = Arc::new(
            OpenAiProvider::new(&args.llm_base_url, args.llm_api_key.as_deref(), cheap_model)
                .context("build cheap provider")?
                .with_omit_temperature(args.llm_omit_temperature),
        );
        router = router.with(ModelTier::Cheap, cheap);
    }
    Ok(router)
}

/// Load, review and (optionally) verify one fixture, recording any
/// failure in the result instead of aborting the run.
async fn run_one(path: &Path, llm: &LlmRouter, verifier_enabled: bool) -> FixtureResult {
    let raw = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            return FixtureResult {
                name: path.display().to_string(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some(format!("read fixture: {err}")),
                label_match: None,
            };
        }
    };
    let fixture: Fixture = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            return FixtureResult {
                name: path.display().to_string(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some(format!("parse fixture: {err}")),
                label_match: None,
            };
        }
    };

    let nonce = fence_nonce(&fixture.name, process_nonce_salt());
    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: &fixture.repo_full_name,
        pr_number: fixture.pr_number,
        pr_title: &fixture.pr_title,
        pr_body: &fixture.pr_body,
        diff: &fixture.diff,
        changed_files: &fixture.changed_files,
        previous_review_sha: None,
        guidelines: &fixture.guidelines,
        repo_context: &fixture.repo_context,
        prior_discussion: "",
        fence_nonce: &nonce,
    });

    let started = Instant::now();
    let initial =
        match generate_with_self_heal(llm, system_prompt(), &prompt, HealConfig::default()).await {
            Ok(output) => output,
            Err(err) => {
                return FixtureResult {
                    name: fixture.name,
                    findings_initial: 0,
                    findings_after_verify: 0,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(format!("review LLM call failed: {err}")),
                    label_match: None,
                };
            }
        };
    let findings_initial = initial.findings.len();

    let scored_output = if verifier_enabled {
        match verify_findings(llm, initial, &fixture.diff).await {
            Ok(output) => output,
            Err(err) => {
                return FixtureResult {
                    name: fixture.name,
                    findings_initial,
                    findings_after_verify: findings_initial,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(format!("verifier failed: {err}")),
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
    let mut matched: usize = 0;
    let mut spurious: usize = 0;

    for finding in &output.findings {
        let hit = expected
            .iter()
            .zip(claimed.iter_mut())
            .find(|&(label, &mut taken)| {
                !taken && label.path == finding.path && label.line == finding.line_start
            });
        if let Some((_, taken)) = hit {
            *taken = true;
            matched = matched.saturating_add(1);
        } else {
            spurious = spurious.saturating_add(1);
        }
    }

    let missed = claimed.iter().filter(|taken| !**taken).count();
    LabelMatch {
        expected: expected.len(),
        matched,
        missed,
        spurious,
    }
}

/// Summarise per-fixture results into the run aggregate.
fn aggregate(results: &[FixtureResult]) -> Aggregate {
    let successes = results
        .iter()
        .filter(|result| result.error.is_none())
        .count();
    let failures = results.len().saturating_sub(successes);
    let total_initial: usize = results.iter().map(|result| result.findings_initial).sum();
    let total_verified: usize = results
        .iter()
        .map(|result| result.findings_after_verify)
        .sum();

    let mut latencies: Vec<u128> = results
        .iter()
        .filter(|result| result.error.is_none())
        .map(|result| result.latency_ms)
        .collect();
    latencies.sort_unstable();
    let mean = u128::try_from(latencies.len())
        .ok()
        .and_then(|count| latencies.iter().sum::<u128>().checked_div(count))
        .unwrap_or(0);
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

/// Precision and recall over every labelled fixture; `None` when no
/// fixture is labelled.
fn compute_label_score(results: &[FixtureResult]) -> Option<LabelScore> {
    let labelled: Vec<&LabelMatch> = results
        .iter()
        .filter_map(|result| result.label_match.as_ref())
        .collect();
    if labelled.is_empty() {
        return None;
    }
    let expected_total: usize = labelled.iter().map(|label| label.expected).sum();
    let matched_total: usize = labelled.iter().map(|label| label.matched).sum();
    let missed_total: usize = labelled.iter().map(|label| label.missed).sum();
    let spurious_total: usize = labelled.iter().map(|label| label.spurious).sum();
    // Precision = TP / (TP + FP). FP = spurious.
    let precision = ratio(matched_total, matched_total.saturating_add(spurious_total));
    // Recall = TP / (TP + FN). FN = missed.
    let recall = ratio(matched_total, matched_total.saturating_add(missed_total));
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
#[expect(
    clippy::float_arithmetic,
    reason = "nearest-rank percentile is defined as ceil(pct / 100 * n), which is float math"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "n is a fixture count, far below the 2^52 limit where f64 stops representing integers exactly"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the rank is ceil of a value clamped to [0, n], so it fits usize, and is clamped to the last index anyway"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "pct is clamped to [0, 100] and n is non-negative, so the rank is never negative"
)]
#[expect(
    clippy::as_conversions,
    reason = "there is no lossless conversion between usize and f64; the bounds above make both casts exact"
)]
fn percentile(sorted: &[u128], pct: f64) -> u128 {
    let Some(last_index) = sorted.len().checked_sub(1) else {
        return 0;
    };
    let clamped_pct = pct.clamp(0.0, 100.0);
    let count = sorted.len() as f64;
    let rank = (clamped_pct / 100.0 * count).ceil() as usize;
    let idx = rank.saturating_sub(1).min(last_index);
    sorted.get(idx).copied().unwrap_or(0)
}

/// Print one fixture's report row.
#[expect(
    clippy::print_stdout,
    reason = "`bench run` reports one row per fixture on stdout"
)]
fn print_row(result: &FixtureResult) {
    if let Some(err) = result.error.as_ref() {
        println!("{:>40}  ERROR  {err}", result.name);
        return;
    }
    let base = format!(
        "{:>40}  {:>3} findings  → {:>3} after verify  {:>6} ms",
        result.name, result.findings_initial, result.findings_after_verify, result.latency_ms,
    );
    if let Some(label) = result.label_match.as_ref() {
        println!(
            "{base}   labels: {}/{} matched, {} missed, {} spurious",
            label.matched, label.expected, label.missed, label.spurious
        );
    } else {
        println!("{base}");
    }
}

/// Print the human-readable run aggregate.
#[expect(
    clippy::print_stdout,
    reason = "`bench run` reports the aggregate table on stdout"
)]
fn print_aggregate(agg: &Aggregate, verifier_enabled: bool) {
    println!();
    println!("\u{2500}\u{2500}\u{2500} Aggregate \u{2500}\u{2500}\u{2500}");
    println!("  fixtures:                    {}", agg.fixtures);
    println!("  successes:                   {}", agg.successes);
    println!("  failures:                    {}", agg.failures);
    println!(
        "  total findings (initial):    {}",
        agg.total_findings_initial
    );
    if verifier_enabled {
        println!(
            "  total findings (verified):   {}",
            agg.total_findings_after_verify
        );
        let dropped = agg
            .total_findings_initial
            .saturating_sub(agg.total_findings_after_verify);
        println!("  dropped by verifier:         {dropped}");
    }
    println!("  mean latency:                {} ms", agg.mean_latency_ms);
    println!(
        "  median latency:              {} ms",
        agg.median_latency_ms
    );
    println!("  p99 latency:                 {} ms", agg.p99_latency_ms);
    if let Some(score) = agg.label_score.as_ref() {
        println!();
        println!(
            "─── Label scoring ({} labelled fixture(s)) ───",
            score.labelled_fixtures
        );
        println!("  expected total:              {}", score.expected_total);
        println!("  matched:                     {}", score.matched_total);
        println!("  missed:                      {}", score.missed_total);
        println!("  spurious:                    {}", score.spurious_total);
        println!("  precision:                   {:.3}", score.precision);
        println!("  recall:                      {:.3}", score.recall);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::Write as _;

    fn agg(
        fixtures: usize,
        successes: usize,
        mean_ms: u128,
        p99_ms: u128,
        findings: usize,
        label: Option<LabelScore>,
    ) -> Aggregate {
        Aggregate {
            fixtures,
            successes,
            failures: fixtures.saturating_sub(successes),
            total_findings_initial: findings,
            total_findings_after_verify: findings,
            mean_latency_ms: mean_ms,
            median_latency_ms: mean_ms,
            p99_latency_ms: p99_ms,
            label_score: label,
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "test helper: derives a small whole fixture count from a precision in [0, 1]; truncation is the intended rounding"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "test helper: every precision the tests pass is in [0, 1], so the product is never negative"
    )]
    #[expect(
        clippy::as_conversions,
        reason = "test helper: f64 has no lossless conversion to usize; the cast rounds a small non-negative fixture count"
    )]
    #[expect(
        clippy::float_arithmetic,
        reason = "test helper: scales a precision ratio to a matched count over five fixtures"
    )]
    fn label(precision: f64, recall: f64) -> LabelScore {
        LabelScore {
            labelled_fixtures: 5,
            expected_total: 5,
            matched_total: (precision * 5.0) as usize,
            missed_total: 0,
            spurious_total: 0,
            precision,
            recall,
        }
    }

    #[expect(
        clippy::default_numeric_fallback,
        reason = "test code: untyped literals in assertions and JSON fixtures; a suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
    )]
    #[test]
    fn compare_no_change_returns_zero_deltas() {
        let baseline = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        let comparison = compare(&baseline, &current);
        assert_eq!(comparison.success_rate_delta, Some(0.0));
        assert_eq!(comparison.precision_delta, Some(0.0));
        assert_eq!(comparison.recall_delta, Some(0.0));
        assert_eq!(comparison.mean_latency_ms_delta, 0);
        assert_eq!(comparison.p99_latency_ms_delta, 0);
        assert_eq!(comparison.total_findings_delta, 0);
        assert!(!comparison.is_regression());
    }

    #[expect(
        clippy::default_numeric_fallback,
        reason = "test code: untyped literals in assertions and JSON fixtures; a suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    #[test]
    fn compare_precision_improvement_is_positive_delta() {
        let baseline = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.9, 0.7)));
        let comparison = compare(&baseline, &current);
        assert!((comparison.precision_delta.unwrap() - 0.1).abs() < 1e-9);
        assert!(!comparison.is_regression());
    }

    #[test]
    fn compare_precision_drop_above_5pp_is_a_regression() {
        let baseline = agg(5, 5, 1000, 5000, 10, Some(label(0.9, 0.7)));
        // precision drops by 10pp.
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        let comparison = compare(&baseline, &current);
        assert!(comparison.is_regression());
    }

    #[test]
    fn compare_precision_drop_below_5pp_is_not_a_regression() {
        let baseline = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        // precision drops 4pp — under the 5pp threshold.
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.76, 0.7)));
        let comparison = compare(&baseline, &current);
        assert!(
            !comparison.is_regression(),
            "4pp drop should not be a regression"
        );
    }

    #[test]
    fn compare_recall_drop_above_5pp_is_a_regression() {
        let baseline = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.9)));
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.8)));
        let comparison = compare(&baseline, &current);
        assert!(comparison.is_regression());
    }

    #[test]
    fn compare_success_rate_drop_above_5pp_is_a_regression() {
        // Catches the failure mode where a model change broke
        // JSON-schema output frequently — the surviving runs may
        // still have decent precision/recall, but the bot is
        // posting fewer reviews overall. Previously the comparison
        // computed `success_rate_delta` but `is_regression()`
        // didn't consult it, so this kind of regression slipped
        // through `--fail-on-regression`.
        let baseline = agg(10, 10, 1000, 5000, 10, None);
        // 5 successes / 10 fixtures = 0.5 success rate (down from
        // 1.0 = 50pp drop, well above the 5pp threshold).
        let current = agg(10, 5, 1000, 5000, 10, None);
        let comparison = compare(&baseline, &current);
        assert!(
            comparison.is_regression(),
            "50pp success-rate drop must trigger regression"
        );
    }

    #[test]
    fn compare_success_rate_drop_below_5pp_is_not_a_regression() {
        // 100 fixtures, 99 vs 96 successes → 3pp drop, under threshold.
        let baseline = agg(100, 99, 1000, 5000, 10, None);
        let current = agg(100, 96, 1000, 5000, 10, None);
        let comparison = compare(&baseline, &current);
        assert!(
            !comparison.is_regression(),
            "3pp drop should not be a regression"
        );
    }

    #[test]
    fn compare_p99_jump_above_5s_is_a_regression() {
        let baseline = agg(5, 5, 1000, 2000, 10, None);
        let current = agg(5, 5, 1000, 8000, 10, None); // +6s
        let comparison = compare(&baseline, &current);
        assert!(comparison.is_regression());
    }

    #[test]
    fn compare_handles_unlabelled_baseline() {
        let baseline = agg(5, 5, 1000, 5000, 10, None);
        let current = agg(5, 5, 1000, 5000, 10, Some(label(0.8, 0.7)));
        let comparison = compare(&baseline, &current);
        assert!(comparison.precision_delta.is_none());
        assert!(comparison.recall_delta.is_none());
        // Latency comparable; not a regression.
        assert!(!comparison.is_regression());
    }

    #[test]
    fn fmt_pct_delta_renders_signed_percentage_points() {
        assert_eq!(fmt_pct_delta(0.05), "+5.00pp");
        assert_eq!(fmt_pct_delta(-0.075), "-7.50pp");
    }

    #[test]
    fn fmt_ms_delta_renders_signed_ms() {
        assert_eq!(fmt_ms_delta(123), "+123ms");
        assert_eq!(fmt_ms_delta(-50), "-50ms");
    }

    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "test code: shipped fixture and scratch file names are lowercase, and the check matches them exactly as written"
    )]
    #[expect(
        clippy::panic,
        reason = "test code: panicking is how this test reports an unexpected variant or outcome"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    /// Contract test: every shipped `bench/fixtures/labelled-*.json`
    /// parses cleanly into a `Fixture` AND has a non-empty `expected`
    /// array (otherwise it's not a labelled fixture and the
    /// filename is misleading). Catches schema drift when someone
    /// renames a field on `Fixture` without updating the fixtures.
    #[test]
    fn shipped_labelled_fixtures_parse_with_expected_findings() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("bench/fixtures");
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("labelled-") && name.ends_with(".json"))
            })
            .collect();
        assert!(
            entries.len() >= 5,
            "expected ≥5 labelled fixtures, found {}",
            entries.len()
        );
        for entry in entries {
            let path = entry.path();
            let body = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            let fix: Fixture = serde_json::from_str(&body)
                .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
            assert!(
                !fix.expected.is_empty(),
                "{}: labelled fixture has empty `expected` array",
                path.display()
            );
            assert!(!fix.diff.is_empty(), "{}: empty diff", path.display());
            // Every expected entry must reference a path that's in
            // the changed_files list (otherwise the model can never
            // possibly hit it through normal review).
            for exp in &fix.expected {
                assert!(
                    fix.changed_files.iter().any(|changed| changed == &exp.path),
                    "{}: expected path {} not in changed_files",
                    path.display(),
                    exp.path
                );
            }
        }
    }

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
                    title: String::new(),
                    category: ar_prompts::FindingCategory::Correctness,
                },
                ReviewFinding {
                    path: "src/util.rs".into(),
                    line_start: 7,
                    line_end: None,
                    severity: ReviewSeverity::Note,
                    message: "y".into(),
                    title: String::new(),
                    category: ar_prompts::FindingCategory::Correctness,
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
        let label_match = score_against_labels(&output, &expected);
        // First model finding matches first expected. Second model
        // finding is spurious. Second expected is missed.
        assert_eq!(label_match.expected, 2);
        assert_eq!(label_match.matched, 1);
        assert_eq!(label_match.missed, 1);
        assert_eq!(label_match.spurious, 1);
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
                    title: String::new(),
                    category: ar_prompts::FindingCategory::Correctness,
                },
                ReviewFinding {
                    path: "x.rs".into(),
                    line_start: 1,
                    line_end: None,
                    severity: ReviewSeverity::Warning,
                    message: "b".into(),
                    title: String::new(),
                    category: ar_prompts::FindingCategory::Correctness,
                },
            ],
        };
        let expected = vec![ExpectedFinding {
            path: "x.rs".into(),
            line: 1,
            note: String::new(),
        }];
        let label_match = score_against_labels(&output, &expected);
        assert_eq!(label_match.matched, 1);
        assert_eq!(label_match.spurious, 1);
        assert_eq!(label_match.missed, 0);
    }

    #[expect(
        clippy::default_numeric_fallback,
        reason = "test code: untyped literals in assertions and JSON fixtures; a suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
    )]
    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
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

    #[expect(
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    #[test]
    fn expand_fixture_paths_handles_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.json");
        let f2 = dir.path().join("b.json");
        let other = dir.path().join("c.txt");
        for file_path in [&f1, &f2, &other] {
            let mut fh = fs::File::create(file_path).unwrap();
            fh.write_all(b"{}").unwrap();
        }
        let extra = dir.path().join("explicit.json");
        fs::File::create(&extra).unwrap().write_all(b"{}").unwrap();

        let resolved = expand_fixture_paths(&[dir.path().into(), extra.clone()]).unwrap();

        // dedup: explicit.json was added twice (via dir + explicit) but
        // shouldn't appear twice in the result. The .txt file is dropped.
        let names: Vec<&str> = resolved
            .iter()
            .map(|resolved_path| resolved_path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"a.json"));
        assert!(names.contains(&"b.json"));
        assert!(names.contains(&"explicit.json"));
        assert!(!names.contains(&"c.txt"));
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "explicit.json")
                .count(),
            1,
            "explicit.json appeared twice"
        );
    }

    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "test code: shipped fixture and scratch file names are lowercase, and the check matches them exactly as written"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    #[test]
    fn expand_fixture_paths_recurses_into_subdirectories() {
        // Operators commonly group fixtures by category
        // (`fixtures/sqli/case1.json`, `fixtures/xss/case1.json`).
        // A non-recursive walk would silently return zero
        // matches when handed `fixtures/`.
        let dir = tempfile::tempdir().unwrap();
        let sqli = dir.path().join("sqli");
        let xss = dir.path().join("xss");
        let nested = sqli.join("more");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&xss).unwrap();
        for fixture in [
            sqli.join("case1.json"),
            xss.join("case1.json"),
            nested.join("case2.json"),
        ] {
            fs::File::create(&fixture)
                .unwrap()
                .write_all(b"{}")
                .unwrap();
        }
        // Drop a non-JSON file in there too — should be ignored.
        fs::File::create(sqli.join("README.md"))
            .unwrap()
            .write_all(b"docs")
            .unwrap();

        let resolved = expand_fixture_paths(&[dir.path().into()]).unwrap();
        let names: Vec<String> = resolved
            .iter()
            .map(|resolved_path| {
                resolved_path
                    .strip_prefix(dir.path())
                    .unwrap()
                    .display()
                    .to_string()
                    .replace('\\', "/")
            })
            .collect();
        assert!(names.iter().any(|name| name == "sqli/case1.json"));
        assert!(names.iter().any(|name| name == "xss/case1.json"));
        assert!(names.iter().any(|name| name == "sqli/more/case2.json"));
        assert!(!names.iter().any(|name| name.ends_with(".md")));
        assert_eq!(resolved.len(), 3);
    }

    #[expect(
        clippy::assertions_on_result_states,
        reason = "test code: the assertion only checks that the call fails and stays as written; the error value is not part of the contract under test"
    )]
    #[test]
    fn expand_fixture_paths_errors_on_nonexistent_path() {
        let result = expand_fixture_paths(&[PathBuf::from("/no/such/path/xyz")]);
        assert!(result.is_err());
    }

    #[expect(
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    fn args_with(llm_base_url: &str, llm_model: &str) -> BenchArgs {
        // Build a minimal BenchArgs we can hand to `run` to test
        // its validation gates. The fixture path needs to exist so
        // expand_fixture_paths doesn't bail before our checks fire.
        let dir = tempfile::tempdir().unwrap();
        let fixture_path = dir.path().join("fixture.json");
        fs::File::create(&fixture_path)
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        // Leak the tempdir so the file outlives the test scope.
        let _kept_dir = dir.keep();
        BenchArgs {
            fixtures: vec![fixture_path],
            llm_base_url: llm_base_url.into(),
            llm_api_key: None,
            llm_model: llm_model.into(),
            llm_omit_temperature: false,
            llm_cheap_model: None,
            json: true,
            baseline: None,
            fail_on_regression: false,
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[tokio::test]
    async fn build_router_omits_temperature_on_reasoning_and_cheap_tiers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            })))
            .mount(&server)
            .await;
        let mut args = args_with(&server.uri(), "gpt-5.1");
        args.llm_cheap_model = Some("gpt-5.1-mini".into());
        args.llm_omit_temperature = true;
        let router = build_router(&args).expect("router");
        for tier in [ModelTier::Reasoning, ModelTier::Cheap] {
            router
                .complete(
                    tier,
                    ar_llm::CompleteRequest {
                        messages: vec![ar_llm::Message::user("x")],
                        temperature: Some(0.0),
                        ..Default::default()
                    },
                )
                .await
                .expect("completion");
        }
        let requests = server.received_requests().await.expect("recording");
        assert_eq!(requests.len(), 2);
        for request in requests {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            assert!(body.get("temperature").is_none(), "{body}");
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[tokio::test]
    async fn run_bails_on_empty_llm_base_url() {
        // clap's `env = "LLM_BASE_URL"` accepts `LLM_BASE_URL=`
        // and overrides any default with an empty string. Without
        // the explicit empty check in `run`, OpenAiProvider would
        // be built with empty base URL and 400 every fixture.
        let args = args_with("", "qwen2.5-coder:32b");
        let err = run(args).await.expect_err("must reject empty base URL");
        assert!(
            err.to_string().contains("--llm-base-url"),
            "expected llm-base-url message, got: {err}"
        );
    }

    #[expect(
        clippy::expect_used,
        reason = "test code: a failed setup, parse or call step must fail this test at once with the expect message"
    )]
    #[tokio::test]
    async fn run_bails_on_empty_llm_model() {
        let args = args_with("https://api.example", "");
        let err = run(args).await.expect_err("must reject empty model");
        assert!(
            err.to_string().contains("--llm-model"),
            "expected llm-model message, got: {err}"
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
        clippy::unwrap_used,
        reason = "test code: a failed setup step must fail this test at the line that broke"
    )]
    /// The shipped `bench/fixtures/*.json` files double as documentation
    /// of the fixture format. If the `Fixture` struct shape changes in a
    /// way that breaks them, the bench harness silently rejects every
    /// fixture in the wild as "parse fixture" errors. This test catches
    /// that at PR time instead of at-deploy time.
    #[test]
    fn shipped_fixtures_all_parse() {
        let workspace_root = env::var("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .ok()
            .and_then(|manifest_dir| {
                // CARGO_MANIFEST_DIR is .../crates/ar-cli; the bench
                // directory lives at the workspace root.
                manifest_dir.parent()?.parent().map(Path::to_path_buf)
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

        let entries = fs::read_dir(&fixtures_dir).expect("read bench/fixtures/");
        let mut count: usize = 0;
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            let _: Fixture = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
            count += 1;
        }
        assert!(count > 0, "no fixtures matched bench/fixtures/*.json");
    }
}
