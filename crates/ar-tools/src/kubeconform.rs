//! kubeconform runner. Parses `kubeconform -output json` output for
//! Kubernetes manifest validation against the upstream JSON Schema.
//!
//! kubeconform is the actively-maintained drop-in for kubeval (which
//! has been unmaintained since 2022). It validates Kubernetes
//! resources without needing a live cluster — schema is fetched from
//! kubernetes-json-schema.
//!
//! Output structure: `{summary: {…}, resources: [{filename, status,
//! kind?, name?, msg?}]}`. We only surface findings whose status is
//! `"invalid"` or `"error"`; `"skipped"` (not a recognised K8s
//! resource) and `"valid"` produce no Findings. yamllint is
//! complementary — it catches YAML-level mistakes; kubeconform
//! catches K8s-level ones.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "kubeconform";

#[derive(Debug, Deserialize)]
struct KubeconformOutput {
    #[serde(default)]
    resources: Vec<KubeconformResource>,
}

#[derive(Debug, Deserialize)]
struct KubeconformResource {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

pub fn parse_kubeconform_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: KubeconformOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for r in raw.resources {
        let lower = r.status.to_ascii_lowercase();
        if !matches!(lower.as_str(), "invalid" | "error") {
            continue;
        }
        let kind_label = r.kind.as_deref().unwrap_or("(unknown kind)");
        let name_label = r.name.as_deref().unwrap_or("");
        let header = if name_label.is_empty() {
            kind_label.to_string()
        } else {
            format!("{kind_label}/{name_label}")
        };
        let detail = r.msg.unwrap_or_default();
        let message = if detail.is_empty() {
            format!("kubeconform: {lower} flagged {header}")
        } else {
            format!("{header}: {detail}")
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: r.kind,
            path: r.filename,
            // kubeconform doesn't carry source-line info in its
            // JSON; surface at line 1 of the manifest. The LLM
            // reads kind/name from the message and locates the
            // resource itself.
            line_start: 1,
            line_end: 1,
            severity: severity_from(&lower),
            message,
        });
    }
    Ok(out)
}

fn severity_from(status_lower: &str) -> Severity {
    match status_lower {
        "error" => Severity::Error,
        "invalid" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct KubeconformRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for KubeconformRunner {
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
        // -strict treats CRD-only fields as errors; -summary off
        // suppresses the trailing summary that would split the
        // JSON. -ignore-missing-schemas keeps missing CRDs from
        // failing the run (they're fine, just not validated).
        let mut args = vec![
            "-output".into(),
            "json".into(),
            "-summary".into(),
            "-ignore-missing-schemas".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "kubeconform", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_kubeconform_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_kubeconform_output() {
        let json = r#"{
            "summary": {"valid": 1, "invalid": 1, "errors": 0, "skipped": 0},
            "resources": [
                {
                    "filename": "k8s/deploy.yaml",
                    "kind": "Deployment",
                    "name": "api",
                    "status": "valid",
                    "msg": ""
                },
                {
                    "filename": "k8s/service.yaml",
                    "kind": "Service",
                    "name": "api",
                    "status": "invalid",
                    "msg": "spec.ports: required property is missing"
                },
                {
                    "filename": "k8s/cronjob.yaml",
                    "kind": "CronJob",
                    "name": "rotate",
                    "status": "skipped",
                    "msg": ""
                }
            ]
        }"#;
        let f = parse_kubeconform_output(json).expect("ok");
        // Only the invalid resource produces a finding.
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "k8s/service.yaml");
        assert_eq!(f[0].rule_id.as_deref(), Some("Service"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Service/api"));
        assert!(f[0].message.contains("required property is missing"));
    }

    #[test]
    fn error_status_maps_to_severity_error() {
        let json = r#"{
            "resources": [{
                "filename":"x.yaml","kind":"Pod","name":"p","status":"error",
                "msg":"unable to load schema"
            }]
        }"#;
        let f = parse_kubeconform_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn unknown_kind_falls_back_to_placeholder_label() {
        let json = r#"{
            "resources": [{
                "filename":"x.yaml","status":"invalid",
                "msg":"missing apiVersion"
            }]
        }"#;
        let f = parse_kubeconform_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("(unknown kind)"));
    }

    #[test]
    fn empty_resources_yields_zero_findings() {
        let f = parse_kubeconform_output(r#"{"resources":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_resources_field_decodes_to_empty() {
        let f = parse_kubeconform_output(r#"{}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_kubeconform_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_msg_falls_back_to_status_summary() {
        let json = r#"{
            "resources": [{
                "filename":"x.yaml","kind":"Pod","name":"p","status":"invalid"
            }]
        }"#;
        let f = parse_kubeconform_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("invalid"));
    }
}
