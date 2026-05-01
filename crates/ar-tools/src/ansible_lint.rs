//! ansible-lint runner. Parses `ansible-lint -f json` output for
//! Ansible playbook / role / task issues.
//!
//! Routes alongside yamllint and kubeconform on every YAML file.
//! ansible-lint cleanly skips YAML that isn't an Ansible artefact
//! (no playbook header, no role layout) — output for non-Ansible
//! input is an empty array, so the cost on non-Ansible repos is
//! one container spawn per review.
//!
//! Output structure: a top-level array of `{type, check_name,
//! severity, description, location: {path, lines: {begin}}}`.
//! Severity values: `blocker`/`critical` → Error,
//! `major`/`minor` → Warning, `info`/anything else → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "ansible-lint";

#[derive(Debug, Deserialize)]
struct AnsibleLintIssue {
    #[serde(default)]
    check_name: Option<String>,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: AnsibleLintLocation,
}

#[derive(Debug, Default, Deserialize)]
struct AnsibleLintLocation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    lines: AnsibleLintLines,
}

#[derive(Debug, Default, Deserialize)]
struct AnsibleLintLines {
    #[serde(default)]
    begin: u32,
}

pub fn parse_ansible_lint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<AnsibleLintIssue> =
        serde_json::from_str(json).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: e.to_string(),
        })?;
    Ok(raw
        .into_iter()
        .map(|i| {
            let line = i.location.lines.begin.max(1);
            Finding {
                source_tool: TOOL.into(),
                rule_id: i.check_name,
                path: i.location.path,
                line_start: line,
                line_end: line,
                severity: severity_from(&i.severity),
                message: i.description,
            }
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "blocker" | "critical" => Severity::Error,
        "major" | "minor" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct AnsibleLintRunner {
    /// YAML files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for AnsibleLintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        // -f json    structured output
        // --offline  skip galaxy connectivity attempts (sandbox has
        //            no network anyway)
        // --nocolor  defensive — ANSI bytes would break the JSON
        let mut args = vec![
            "-f".into(),
            "json".into(),
            "--offline".into(),
            "--nocolor".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "ansible-lint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_ansible_lint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_ansible_lint_output() {
        let json = r#"[
            {
                "type": "issue",
                "check_name": "yaml[trailing-spaces]",
                "categories": ["formatting", "yaml"],
                "severity": "minor",
                "description": "Trailing spaces.",
                "location": {
                    "path": "playbooks/site.yml",
                    "lines": {"begin": 12}
                }
            },
            {
                "type": "issue",
                "check_name": "risky-shell-pipe",
                "severity": "blocker",
                "description": "Shell pipe risk: missing pipefail.",
                "location": {
                    "path": "roles/web/tasks/main.yml",
                    "lines": {"begin": 4}
                }
            }
        ]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "playbooks/site.yml");
        assert_eq!(f[0].line_start, 12);
        assert_eq!(f[0].rule_id.as_deref(), Some("yaml[trailing-spaces]"));
        assert_eq!(f[0].severity, Severity::Warning); // minor
        assert_eq!(f[1].severity, Severity::Error); // blocker
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_ansible_lint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_ansible_lint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn info_severity_falls_back_to_note() {
        let json = r#"[{
            "check_name":"R","severity":"info","description":"d",
            "location":{"path":"x.yml","lines":{"begin":1}}
        }]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[{
            "check_name":"R","severity":"trace","description":"d",
            "location":{"path":"x.yml","lines":{"begin":1}}
        }]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_check_name_drops_rule_id() {
        let json = r#"[{
            "severity":"minor","description":"d",
            "location":{"path":"x.yml","lines":{"begin":1}}
        }]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn missing_lines_falls_back_to_one() {
        let json = r#"[{
            "check_name":"R","severity":"minor","description":"d",
            "location":{"path":"x.yml"}
        }]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[{
            "check_name":"R","severity":"minor","description":"d",
            "location":{"path":"x.yml","lines":{"begin":0}}
        }]"#;
        let f = parse_ansible_lint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
    }
}
