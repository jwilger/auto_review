use std::path::Path;

#[test]
fn agentcore_deploy_assets_cover_runtime_container_iam_and_ci_invocation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let asset = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("read {path}: {error}"))
    };

    let containerfile = asset("deploy/agentcore/Containerfile");
    assert!(containerfile.contains("auto-review agentcore serve"));
    assert!(containerfile.contains("EXPOSE 9000"));

    let runtime_config = asset("deploy/agentcore/runtime-config.json");
    assert!(runtime_config.contains("\"port\": 9000"));
    assert!(runtime_config.contains("\"/ping\""));
    assert!(runtime_config.contains("\"/invocations\""));

    let iam_notes = asset("deploy/agentcore/iam-policy.md");
    for required in [
        "AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE",
        "AGENTCORE_HISTORY_DYNAMODB_TABLE",
        "AGENTCORE_LEARNINGS_DYNAMODB_TABLE",
        "dynamodb:PutItem",
        "dynamodb:UpdateItem",
        "dynamodb:DeleteItem",
        "dynamodb:GetItem",
        "dynamodb:Scan",
    ] {
        assert!(iam_notes.contains(required), "missing {required}");
    }

    let github = asset("deploy/agentcore/github-actions-oidc.yml");
    for required in [
        "id-token: write",
        "aws-actions/configure-aws-credentials",
        "provider: github",
        "kind: semantic_review",
        "installation_id",
        "head_sha",
    ] {
        assert!(github.contains(required), "missing {required}");
    }

    let forgejo = asset("deploy/agentcore/forgejo-actions.yml");
    for required in [
        "provider: forgejo",
        "kind: semantic_review",
        "owner",
        "repo",
        "pr_number",
        "head_sha",
    ] {
        assert!(forgejo.contains(required), "missing {required}");
    }

    let readme = asset("deploy/agentcore/README.md");
    assert!(readme.contains("no dedicated gateway"));
    assert!(readme.contains("auto-review agentcore serve"));
    assert!(readme.contains("DynamoDB"));
}
