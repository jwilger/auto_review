use ar_review::parse_repo_config;

#[test]
fn parses_fine_grained_pr_metadata_check_controls() {
    let cfg = parse_repo_config(
        r#"
pr_metadata_check:
  enabled: true
  checks:
    body_required: false
  additional_rules:
    - Security-sensitive changes must describe the threat model impact.
"#,
    )
    .expect("parse config");

    assert!(!cfg.pr_metadata_check.checks.body_required);
}
