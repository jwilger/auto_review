use ar_review::parse_repo_config;

#[test]
#[expect(
    clippy::expect_used,
    reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
fn parses_fine_grained_pr_metadata_check_controls() {
    let cfg = parse_repo_config(
        "
pr_metadata_check:
  enabled: true
  checks:
    body_required: false
  additional_rules:
    - Security-sensitive changes must describe the threat model impact.
",
    )
    .expect("parse config");

    assert!(!cfg.pr_metadata_check.checks.body_required);
}
