use ar_review::{parse_repo_config_strict, RepoConfigStrictError};

#[test]
fn strict_config_rejects_retired_linter_runtime_keys() {
    let err = parse_repo_config_strict("mode: linter_only\ndisabled_tools:\n  - ruff\n")
        .expect_err("retired linter runtime keys should be unknown");

    let RepoConfigStrictError::UnknownKeys(keys) = err else {
        panic!("expected unknown keys error");
    };

    assert_eq!(keys, ["disabled_tools", "mode"]);
}
