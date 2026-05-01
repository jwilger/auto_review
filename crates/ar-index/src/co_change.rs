//! Co-change graph from `git log --name-only`.
//!
//! Two files are co-changed when they appear in the same commit. We
//! count the number of commits each unordered pair appears in together
//! and use that as a relevance signal at review time: "you edited X;
//! Y co-changes with X frequently — here are the matching symbols."
//!
//! This is the cheapest part of CodeRabbit's RAG layer (no embeddings
//! needed) and is useful even before LanceDB lands.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoChangeGraph {
    /// Map from a file path to a map of co-changed-file → count.
    /// Symmetric: if A→B has count N, B→A also has count N.
    pub edges: HashMap<String, HashMap<String, u32>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CoChangeError {
    #[error("git failed with status {status}: {stderr}")]
    Git { status: i32, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl CoChangeGraph {
    /// Files most-frequently co-changed with `path`, descending by count.
    pub fn co_changed_with(&self, path: &str, top_n: usize) -> Vec<(String, u32)> {
        let Some(neighbors) = self.edges.get(path) else {
            return Vec::new();
        };
        let mut pairs: Vec<(String, u32)> =
            neighbors.iter().map(|(k, v)| (k.clone(), *v)).collect();
        // Sort by count desc, then path asc for determinism.
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        pairs.truncate(top_n);
        pairs
    }
}

/// Parse the output of `git log --name-only --pretty=format:'COMMIT %H'`
/// into a co-change graph. Each commit's listed files contribute one
/// count to every unordered pair within that commit.
pub fn parse_git_log_co_change(log: &str) -> CoChangeGraph {
    let mut graph = CoChangeGraph::default();
    let mut current_files: Vec<String> = Vec::new();

    let flush = |graph: &mut CoChangeGraph, files: &mut Vec<String>| {
        if files.len() >= 2 {
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let a = &files[i];
                    let b = &files[j];
                    *graph
                        .edges
                        .entry(a.clone())
                        .or_default()
                        .entry(b.clone())
                        .or_insert(0) += 1;
                    *graph
                        .edges
                        .entry(b.clone())
                        .or_default()
                        .entry(a.clone())
                        .or_insert(0) += 1;
                }
            }
        }
        files.clear();
    };

    for line in log.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(_sha) = line.strip_prefix("COMMIT ") {
            // New commit boundary: flush the previous commit's files.
            flush(&mut graph, &mut current_files);
        } else {
            current_files.push(line.to_string());
        }
    }
    // Flush the final commit.
    flush(&mut graph, &mut current_files);

    graph
}

/// Run `git log` against `repo_dir` and parse the output.
///
/// `lookback_commits` caps how far back we look. Higher values give a
/// richer graph at the cost of git-log time and memory; 1000 is a
/// reasonable default for medium-sized repos.
pub fn compute_co_change(
    repo_dir: &Path,
    lookback_commits: u32,
) -> Result<CoChangeGraph, CoChangeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args([
            "log",
            "--name-only",
            "--pretty=format:COMMIT %H",
            &format!("-n{lookback_commits}"),
        ])
        .output()?;
    if !output.status.success() {
        return Err(CoChangeError::Git {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    let log = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_log_co_change(&log))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_commit_with_one_file_yields_no_edges() {
        let log = "COMMIT abc\nsrc/main.rs\n";
        let g = parse_git_log_co_change(log);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn single_commit_with_two_files_yields_one_pair() {
        let log = "COMMIT abc\nsrc/main.rs\nsrc/lib.rs\n";
        let g = parse_git_log_co_change(log);
        assert_eq!(g.edges["src/main.rs"]["src/lib.rs"], 1);
        assert_eq!(g.edges["src/lib.rs"]["src/main.rs"], 1);
    }

    #[test]
    fn multiple_commits_accumulate_counts() {
        let log = "\
COMMIT a
src/main.rs
src/lib.rs
COMMIT b
src/main.rs
src/lib.rs
COMMIT c
src/main.rs
README.md
";
        let g = parse_git_log_co_change(log);
        assert_eq!(g.edges["src/main.rs"]["src/lib.rs"], 2);
        assert_eq!(g.edges["src/main.rs"]["README.md"], 1);
        // README.md co-changes with main but not lib.
        assert_eq!(g.edges.get("README.md").unwrap().get("src/lib.rs"), None);
    }

    #[test]
    fn three_files_in_one_commit_create_three_pairs() {
        let log = "COMMIT abc\na.rs\nb.rs\nc.rs\n";
        let g = parse_git_log_co_change(log);
        // Pairs: (a,b), (a,c), (b,c) — three undirected, six directed
        assert_eq!(g.edges["a.rs"]["b.rs"], 1);
        assert_eq!(g.edges["a.rs"]["c.rs"], 1);
        assert_eq!(g.edges["b.rs"]["c.rs"], 1);
        assert_eq!(g.edges["b.rs"]["a.rs"], 1);
        assert_eq!(g.edges["c.rs"]["a.rs"], 1);
        assert_eq!(g.edges["c.rs"]["b.rs"], 1);
    }

    #[test]
    fn co_changed_with_returns_top_n_by_count_desc() {
        let log = "\
COMMIT a
x.rs
high_friend.rs
COMMIT b
x.rs
high_friend.rs
COMMIT c
x.rs
mid_friend.rs
COMMIT d
x.rs
low_friend.rs
";
        let g = parse_git_log_co_change(log);
        let top2 = g.co_changed_with("x.rs", 2);
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0], ("high_friend.rs".into(), 2));
        // Second slot is whichever 1-count file sorts first alphabetically.
        assert_eq!(top2[1].1, 1);
    }

    #[test]
    fn co_changed_with_returns_empty_for_unknown_path() {
        let log = "COMMIT a\nx.rs\ny.rs\n";
        let g = parse_git_log_co_change(log);
        assert!(g.co_changed_with("nonexistent.rs", 5).is_empty());
    }

    #[test]
    fn empty_log_yields_empty_graph() {
        let g = parse_git_log_co_change("");
        assert!(g.edges.is_empty());
    }

    #[test]
    fn trailing_blank_lines_are_tolerated() {
        let log = "COMMIT a\nfoo.rs\nbar.rs\n\n\n";
        let g = parse_git_log_co_change(log);
        assert_eq!(g.edges["foo.rs"]["bar.rs"], 1);
    }
}
