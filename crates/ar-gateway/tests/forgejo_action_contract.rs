const ACTION_YAML: &str = include_str!("../../../deploy/forgejo-action/action.yml");
const CI_YAML: &str = include_str!("../../../.forgejo/workflows/ci.yml");

#[test]
fn forgejo_action_posts_ci_review_to_gateway() {
    let mut contract_errors = Vec::new();
    let compact_action = ACTION_YAML.split_whitespace().collect::<String>();

    for input in [
        "gateway-url",
        "action-token",
        "owner",
        "repo",
        "pr-number",
        "head-sha",
    ] {
        require(
            &mut contract_errors,
            ACTION_YAML.contains(&format!("  {input}:")),
            format!("action.yml should expose the `{input}` input"),
        );
    }

    for input in ["owner", "repo", "pr-number", "head-sha"] {
        let block = input_block(input);
        let lower_block = block.to_lowercase();
        require(
            &mut contract_errors,
            !block.contains("required: true") || lower_block.contains("override"),
            format!("action.yml input `{input}` should be optional or documented as an override"),
        );
    }

    for legacy_input in [
        "forgejo-token",
        "llm-base-url",
        "llm-api-key",
        "llm-reasoning-model",
        "llm-embedding-model",
        "llm-cheap-model",
    ] {
        require(
            &mut contract_errors,
            !ACTION_YAML.contains(&format!("  {legacy_input}:")),
            format!("action.yml should not expose legacy local-review input `{legacy_input}`"),
        );
    }

    require(
        &mut contract_errors,
        compact_action.contains("${{inputs.gateway-url}}/reviews/ci")
            || compact_action.contains("$GATEWAY_URL/reviews/ci")
            || compact_action.contains("${GATEWAY_URL}/reviews/ci"),
        "action.yml should bind the gateway URL to the /reviews/ci endpoint URL",
    );
    require(
        &mut contract_errors,
        compact_action.contains("-XPOST") || compact_action.contains("--requestPOST"),
        "action.yml should perform an HTTP POST request",
    );
    require(
        &mut contract_errors,
        compact_action.contains("ACTION_TOKEN:${{inputs.action-token}}")
            || compact_action.contains("ACTION_TOKEN=\"${{inputs.action-token}}\"")
            || compact_action.contains("ACTION_TOKEN='${{inputs.action-token}}'")
            || compact_action.contains("ACTION_TOKEN=${{inputs.action-token}}"),
        "action.yml should pass inputs.action-token through a step env or shell variable",
    );
    require(
        &mut contract_errors,
        (compact_action.contains("Authorization:Bearer$ACTION_TOKEN")
            || compact_action.contains("Authorization:Bearer${ACTION_TOKEN}"))
            && !compact_action.contains("Authorization:Bearer${{inputs.action-token}}"),
        "action.yml should build the Authorization header from the action token variable, not the direct input expression",
    );
    for (field, variable) in [
        ("owner", "OWNER"),
        ("repo", "REPO"),
        ("pr_number", "PR_NUMBER"),
        ("head_sha", "HEAD_SHA"),
    ] {
        require(
            &mut contract_errors,
            compact_action.contains(&format!("\"{field}\":\"${variable}\""))
                || compact_action.contains(&format!("\"{field}\":\"${{{variable}}}\"")),
            format!("action.yml should send JSON payload field `{field}` sourced from ${variable}"),
        );
    }
    require(
        &mut contract_errors,
        compact_action.contains("OWNER=${{inputs.owner}}")
            && (compact_action.contains("github.repository_owner")
                || compact_action.contains("GITHUB_REPOSITORY_OWNER")
                || compact_action.contains("${GITHUB_REPOSITORY%%/*}")),
        "action.yml should default OWNER from github.repository_owner or an equivalent Forgejo/GitHub env fallback while allowing inputs.owner override",
    );
    require(
        &mut contract_errors,
        compact_action.contains("REPO=${{inputs.repo}}")
            && (compact_action.contains("github.event.repository.name")
                || compact_action.contains("GITHUB_REPOSITORY##*/")),
        "action.yml should default REPO from github.event.repository.name or an equivalent Forgejo/GitHub env fallback while allowing inputs.repo override",
    );
    require(
        &mut contract_errors,
        compact_action.contains("PR_NUMBER=${{inputs.pr-number}}")
            && (compact_action.contains("github.event.pull_request.number")
                || compact_action.contains("GITHUB_REF")
                || compact_action.contains("refs/pull/")),
        "action.yml should default PR_NUMBER from github.event.pull_request.number or an equivalent Forgejo/GitHub env fallback while allowing inputs.pr-number override",
    );
    require(
        &mut contract_errors,
        compact_action.contains("HEAD_SHA=${{inputs.head-sha}}")
            && (compact_action.contains("github.event.pull_request.head.sha")
                || compact_action.contains("GITHUB_SHA")),
        "action.yml should default HEAD_SHA from github.event.pull_request.head.sha or an equivalent Forgejo/GitHub env fallback while allowing inputs.head-sha override",
    );
    require(
        &mut contract_errors,
        !ACTION_YAML.contains("auto_review review-once"),
        "action.yml should delegate to the gateway instead of running auto_review review-once",
    );
    require(
        &mut contract_errors,
        !ACTION_YAML.contains("cargo build"),
        "action.yml should not build the local CLI",
    );

    let pr_number_input = "${{inputs.pr-number}}";
    let pr_number_shell_var = "PR_NUMBER";
    let assigns_pr_number_from_input = [
        format!("{pr_number_shell_var}=\"{pr_number_input}\""),
        format!("{pr_number_shell_var}='{pr_number_input}'"),
        format!("{pr_number_shell_var}={pr_number_input}"),
    ]
    .iter()
    .any(|assignment| compact_action.contains(assignment));
    let checks_pr_number_input_directly = [
        format!("if[-z\"{pr_number_input}\"]"),
        format!("if[[-z\"{pr_number_input}\"]]"),
    ]
    .iter()
    .any(|check| compact_action.contains(check));
    let checks_assigned_pr_number = assigns_pr_number_from_input
        && [
            format!("if[-z\"${pr_number_shell_var}\"]"),
            format!("if[[-z\"${pr_number_shell_var}\"]]"),
            format!("if[-z\"${{{pr_number_shell_var}}}\"]"),
            format!("if[[-z\"${{{pr_number_shell_var}}}\"]]"),
        ]
        .iter()
        .any(|check| compact_action.contains(check));
    let lower_action = ACTION_YAML.to_lowercase();
    let validates_missing_pr_context = (checks_pr_number_input_directly
        || checks_assigned_pr_number)
        && ACTION_YAML.contains("exit 1")
        && lower_action.contains("pull request context");
    require(
        &mut contract_errors,
        validates_missing_pr_context,
        "action.yml should check missing inputs.pr-number at runtime and exit 1 with a clear pull request context error message",
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn ci_workflow_runs_semantic_review_through_forgejo_action_after_prerequisites() {
    let mut contract_errors = Vec::new();

    let Some(review_job) = job_block_containing(CI_YAML, "deploy/forgejo-action") else {
        panic!("ci.yml should include a semantic review job that uses deploy/forgejo-action");
    };
    let compact_review_job = review_job.split_whitespace().collect::<String>();

    require(
        &mut contract_errors,
        review_job.contains("needs: flake-check")
            || compact_review_job.contains("needs:[flake-check"),
        "semantic review job should need the prerequisite flake-check CI job",
    );
    require(
        &mut contract_errors,
        review_job.contains("gateway-url:")
            && (review_job.contains("secrets.") || review_job.contains("env.")),
        "semantic review job should pass gateway-url from secrets or env",
    );
    require(
        &mut contract_errors,
        review_job.contains("action-token:")
            && (review_job.contains("secrets.") || review_job.contains("env.")),
        "semantic review job should pass action-token from secrets or env",
    );

    for local_check in [
        "nix flake check",
        "cargo fmt",
        "cargo clippy",
        "cargo nextest",
        "cargo deny",
    ] {
        require(
            &mut contract_errors,
            !review_job.contains(local_check),
            format!("semantic review job should not run `{local_check}` locally"),
        );
    }

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

fn input_block(input: &str) -> &str {
    let start_marker = format!("  {input}:");
    let Some(start) = ACTION_YAML.find(&start_marker) else {
        return "";
    };
    let rest = &ACTION_YAML[start..];
    let next_input = rest
        .match_indices("\n  ")
        .find(|(_, candidate)| {
            candidate
                .strip_prefix('\n')
                .unwrap_or(candidate)
                .lines()
                .next()
                .is_some_and(|line| line.ends_with(':'))
        })
        .map(|(index, _)| index)
        .unwrap_or(rest.len());

    &rest[..next_input]
}

fn job_block_containing<'a>(workflow: &'a str, needle: &str) -> Option<&'a str> {
    let mut job_start = None;

    for (index, _) in workflow.match_indices('\n') {
        let line_start = index + 1;
        let line = workflow[line_start..].lines().next().unwrap_or_default();

        if is_job_header(line) {
            if let Some(start) = job_start {
                let block = &workflow[start..index];
                if block.contains(needle) {
                    return Some(block);
                }
            }
            job_start = Some(line_start);
        }
    }

    job_start.and_then(|start| {
        let block = &workflow[start..];
        block.contains(needle).then_some(block)
    })
}

fn is_job_header(line: &str) -> bool {
    let trimmed = line.trim_end();
    let Some(job_key) = trimmed.strip_prefix("  ") else {
        return false;
    };

    !job_key.starts_with(' ') && job_key.ends_with(':') && !job_key.contains(' ')
}

fn require(errors: &mut Vec<String>, condition: bool, message: impl Into<String>) {
    if !condition {
        errors.push(message.into());
    }
}
