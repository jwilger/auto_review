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
}

#[derive(Debug, Serialize)]
struct FixtureResult {
    name: String,
    findings_initial: usize,
    findings_after_verify: usize,
    latency_ms: u128,
    error: Option<String>,
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
                };
            }
        };
    let findings_initial = initial.findings.len();

    let after_verify = if verifier_enabled {
        match verify_findings(llm, initial, &fixture.diff).await {
            Ok(o) => o.findings.len(),
            Err(e) => {
                return FixtureResult {
                    name: fixture.name,
                    findings_initial,
                    findings_after_verify: findings_initial,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(format!("verifier failed: {e}")),
                };
            }
        }
    } else {
        findings_initial
    };

    FixtureResult {
        name: fixture.name,
        findings_initial,
        findings_after_verify: after_verify,
        latency_ms: started.elapsed().as_millis(),
        error: None,
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

    Aggregate {
        fixtures: results.len(),
        successes,
        failures,
        total_findings_initial: total_initial,
        total_findings_after_verify: total_verified,
        mean_latency_ms: mean,
        median_latency_ms: median,
        p99_latency_ms: p99,
    }
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
    } else {
        println!(
            "{:>40}  {:>3} findings  → {:>3} after verify  {:>6} ms",
            r.name, r.findings_initial, r.findings_after_verify, r.latency_ms,
        );
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
            },
            FixtureResult {
                name: "b".into(),
                findings_initial: 1,
                findings_after_verify: 1,
                latency_ms: 200,
                error: None,
            },
            FixtureResult {
                name: "c".into(),
                findings_initial: 0,
                findings_after_verify: 0,
                latency_ms: 0,
                error: Some("boom".into()),
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
}
