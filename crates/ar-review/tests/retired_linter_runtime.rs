use ar_review::{parse_repo_config_strict, RepoConfigStrictError};

#[test]
#[expect(
    clippy::expect_used,
    reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
)]
#[expect(
    clippy::panic,
    reason = "test code: panicking is how this test reports an unexpected variant or outcome"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
fn strict_config_rejects_retired_linter_runtime_keys() {
    let err = parse_repo_config_strict("mode: linter_only\ndisabled_tools:\n  - ruff\n")
        .expect_err("retired linter runtime keys should be unknown");

    let RepoConfigStrictError::UnknownKeys(keys) = err else {
        panic!("expected unknown keys error");
    };

    assert_eq!(keys, ["disabled_tools", "mode"]);
}
