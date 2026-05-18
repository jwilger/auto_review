const CI_WORKFLOW: &str = include_str!("../../../.forgejo/workflows/ci.yml");
const RELEASE_PREPARE_WORKFLOW: &str =
    include_str!("../../../.forgejo/workflows/release-prepare.yml");
const RELEASE_PUBLISH_WORKFLOW: &str =
    include_str!("../../../.forgejo/workflows/release-publish.yml");

#[test]
fn pr_ci_exposes_separate_just_based_deterministic_jobs() {
    let mut contract_errors = Vec::new();

    require(
        &mut contract_errors,
        CI_WORKFLOW.contains("  pull_request:"),
        ".forgejo/workflows/ci.yml should run for pull_request events",
    );

    for gate in ["fmt", "clippy", "test", "deny", "build"] {
        let Some(job) = workflow_job(gate) else {
            contract_errors.push(format!(
                ".forgejo/workflows/ci.yml should expose a separate `{gate}` PR CI job"
            ));
            continue;
        };

        require(
            &mut contract_errors,
            job.contains("pull_request"),
            format!("`{gate}` job should be scoped to pull_request CI"),
        );
        require(
            &mut contract_errors,
            job_contains_run_command(job, &format!("just {gate}")),
            format!("`{gate}` job should run `just {gate}`"),
        );
        require(
            &mut contract_errors,
            !job_contains_run_command(job, "nix flake check"),
            format!("`{gate}` job should not be backed by monolithic `nix flake check`"),
        );
        require(
            &mut contract_errors,
            job_checkout_disables_persisted_credentials(job),
            format!(
                "`{gate}` job should disable persisted checkout credentials before running PR-controlled `just {gate}`"
            ),
        );
    }

    require(
        &mut contract_errors,
        workflow_job("flake-check").is_none(),
        ".forgejo/workflows/ci.yml should not expose a monolithic `flake-check` PR gate",
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn pr_ci_classifies_changed_paths_before_expensive_gates() {
    let mut contract_errors = Vec::new();

    let jobs = workflow_jobs(CI_WORKFLOW);
    let Some((classifier_job_name, _classifier_job, app_output_key, opencode_output_key)) =
        jobs.iter().find_map(|(job_name, job_text)| {
            let output_keys = workflow_job_output_keys(job_text);
            let app_output_key = find_matching_output_key(&output_keys, &["app", "ci"])
                .or_else(|| find_matching_output_key(&output_keys, &["app"]));
            let opencode_output_key = find_matching_output_key(&output_keys, &["opencode", "ci"])
                .or_else(|| find_matching_output_key(&output_keys, &["opencode"]));

            match (app_output_key, opencode_output_key) {
                (Some(app_output_key), Some(opencode_output_key)) => Some((
                    job_name.as_str(),
                    *job_text,
                    app_output_key,
                    opencode_output_key,
                )),
                _ => None,
            }
        })
    else {
        panic!(
            "CI workflow should define a path-classification job that exposes separate app and opencode CI output keys"
        );
    };

    let app_output_ref = format!("needs.{classifier_job_name}.outputs.{app_output_key}");
    let opencode_output_ref = format!("needs.{classifier_job_name}.outputs.{opencode_output_key}");

    for gate in ["fmt", "clippy", "test", "deny", "build"] {
        let Some(job) = workflow_job(gate) else {
            panic!(".forgejo/workflows/ci.yml should expose `{gate}` PR CI job");
        };

        require(
            &mut contract_errors,
            workflow_job_if(job).is_some_and(|if_line| if_line.contains("pull_request")),
            format!("`{gate}` should remain scoped to pull_request events"),
        );
        require(
            &mut contract_errors,
            workflow_job_needs(job).is_some_and(|needs| needs.contains(classifier_job_name)),
            format!("`{gate}` should depend on `{classifier_job_name}` path-classification output"),
        );
        require(
            &mut contract_errors,
            workflow_job_if(job).is_some_and(|if_line| {
                if_line.contains(&app_output_ref) && if_line.contains("'true'")
            }),
            format!("`{gate}` should only run when application paths changed"),
        );
    }

    let Some(opencode_job) = workflow_job("opencode-test") else {
        panic!(".forgejo/workflows/ci.yml should expose an `opencode-test` PR CI job");
    };

    require(
        &mut contract_errors,
        workflow_job_needs(opencode_job).is_some_and(|needs| needs.contains(classifier_job_name)),
        "opencode-test should depend on path-classification outputs so it can be selected from changes".to_string(),
    );
    require(
        &mut contract_errors,
        workflow_job_if(opencode_job).is_some_and(|if_line| {
            if_line.contains(&opencode_output_ref) && if_line.contains("'true'")
        }),
        "opencode-test should run only when opencode-related paths changed".to_string(),
    );

    let Some(semantic_review_job) = workflow_job("semantic-review") else {
        panic!(".forgejo/workflows/ci.yml should expose a `semantic-review` PR job");
    };

    require(
        &mut contract_errors,
        workflow_job_if(semantic_review_job)
            .is_some_and(|if_line| if_line.contains("github.event_name == 'pull_request'")),
        "semantic-review should remain available for every pull_request after path-based routing"
            .to_string(),
    );

    let Some(semantic_needs) = workflow_job_needs(semantic_review_job) else {
        panic!("semantic-review should declare needs to avoid running before path routing");
    };
    let semantic_needs_tokens = workflow_job_needs_tokens(semantic_needs);
    require(
        &mut contract_errors,
        semantic_needs_tokens
            .iter()
            .all(|token| !matches!(token.as_str(), "fmt" | "clippy" | "test" | "deny" | "build")),
        "semantic-review should not depend on full application gates; path-classified opencode-only PRs must still get semantic review", 
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn pr_ci_classifier_treats_root_opencode_json_as_opencode_and_fails_closed_to_app_ci_on_uncertain_diff(
) {
    let mut contract_errors = Vec::new();

    let Some(classifier_job) = workflow_job("path-classification") else {
        panic!(
            ".forgejo/workflows/ci.yml should define path-classification job for PR path routing"
        );
    };

    let Some(detect_paths_step) = workflow_step_lines(classifier_job, "Detect changed paths")
    else {
        panic!("path-classification should include a Detect changed paths step");
    };
    let while_guard_index =
        line_index_with_prefix(&detect_paths_step, "while IFS= read -r file; do");
    let app_ci_initial_true_index = line_index_with_prefix(&detect_paths_step, "app_ci=\"true\"");
    let app_ci_initial_false_index = line_index_with_prefix(&detect_paths_step, "app_ci=\"false\"");
    let case_patterns = shell_case_patterns(&detect_paths_step);

    require(
        &mut contract_errors,
        case_patterns
            .iter()
            .any(|pattern| pattern.contains("opencode.json")),
        "path-classification should treat root opencode.json as an opencode path",
    );

    let fail_closed_to_app_ci = match (
        while_guard_index,
        app_ci_initial_true_index,
        app_ci_initial_false_index,
    ) {
        (Some(while_index), Some(true_index), _) => true_index < while_index,
        (Some(_while_index), None, Some(_false_index)) => false,
        (Some(_while_index), None, None) => false,
        _ => false,
    };

    require(
        &mut contract_errors,
        fail_closed_to_app_ci,
        "path-classification should default app_ci='true' before diff iteration to fail closed when no file list can be resolved",
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn release_prepare_uses_semver_checks_for_release_type_planning() {
    let mut contract_errors = Vec::new();
    let Some(job) = workflow_job_in(RELEASE_PREPARE_WORKFLOW, "release-prepare") else {
        panic!(".forgejo/workflows/release-prepare.yml should expose a `release-prepare` job");
    };

    require(
        &mut contract_errors,
        job.contains("cargo semver-checks")
            && job.contains("--baseline-rev \"$BASELINE_TAG\"")
            && ["patch", "minor", "major"]
                .into_iter()
                .all(|release_type| job.contains(release_type)),
        "release-prepare should use cargo semver-checks with --baseline-rev \"$BASELINE_TAG\" while considering patch, minor, and major release types",
    );
    require(
        &mut contract_errors,
        job_contains_run_command(job, "scripts/release plan --workspace ."),
        "release-prepare should plan release metadata",
    );
    require(
        &mut contract_errors,
        job_contains_run_command(job, "scripts/release prepare --workspace ."),
        "release-prepare should prepare release metadata",
    );
    require(
        &mut contract_errors,
        job_contains_run_command(job, "tea pr create")
            || job_contains_run_command(job, "tea pr edit"),
        "release-prepare should open or update the release PR",
    );
    require(
        &mut contract_errors,
        job_contains_run_command(job, "git push --no-verify --force-with-lease origin \"$branch\""),
        "release-prepare should bypass hook-driven full verification when pushing the generated release branch",
    );
    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn release_prepare_push_bypasses_local_hooks() {
    let Some(job) = workflow_job_in(RELEASE_PREPARE_WORKFLOW, "release-prepare") else {
        panic!(".forgejo/workflows/release-prepare.yml should expose a `release-prepare` job");
    };

    assert!(
        job_contains_run_command(job, "git push --no-verify --force-with-lease origin \"$branch\""),
        "release-prepare should push the generated release branch with --no-verify so checked-out pre-push hooks cannot run"
    );
}

#[test]
fn release_prepare_uses_forgejo_api_json_for_open_pr_lookup() {
    let Some(job) = workflow_job_in(RELEASE_PREPARE_WORKFLOW, "release-prepare") else {
        panic!(".forgejo/workflows/release-prepare.yml should expose a `release-prepare` job");
    };

    assert!(
        !job.contains("tea pr ls")
            && job.contains("/api/v1/repos/jwilger/auto_review/pulls?state=open"),
        "release-prepare should query Forgejo pulls API JSON at /api/v1/repos/jwilger/auto_review/pulls?state=open instead of piping tea pr ls output into jq"
    );
}

#[test]
fn release_prepare_isolates_nix_logs_from_open_pr_json() {
    let Some(job) = workflow_job_in(RELEASE_PREPARE_WORKFLOW, "release-prepare") else {
        panic!(".forgejo/workflows/release-prepare.yml should expose a `release-prepare` job");
    };

    assert!(
        job.contains("open_prs_json=")
            && job.contains("export OPEN_PRS_JSON=\"$open_prs_json\"")
            && job.contains("> \"$OPEN_PRS_JSON\"")
            && job.contains("open_prs=\"$(<\"$OPEN_PRS_JSON\")\"")
            && !job.contains("| nix develop --command jq"),
        "release-prepare should write open PR API JSON through exported $OPEN_PRS_JSON and derive PR IDs inside Nix so Nix stdout chatter cannot corrupt jq input"
    );
}

#[test]
fn release_publish_rejects_non_release_metadata_file_diffs() {
    let mut contract_errors = Vec::new();
    let Some(job) = workflow_job_in(RELEASE_PUBLISH_WORKFLOW, "release-publish") else {
        panic!(".forgejo/workflows/release-publish.yml should expose a `release-publish` job");
    };

    let Some(validation_step) =
        workflow_step_lines(job, "Validate release provenance and changed files")
    else {
        panic!("release-publish should validate allowed file-diff paths before publishing");
    };
    let validation_step = validation_step.join("\n");
    require(
        &mut contract_errors,
        validation_step.contains("case \"$changed_file\" in"),
        "release-publish file-diff validation should remain explicit",
    );
    require(
        &mut contract_errors,
        validation_step.contains("Cargo.toml|Cargo.lock|CHANGELOG.md"),
        "release-publish file-diff guard should explicitly allow Cargo.toml, Cargo.lock, and CHANGELOG.md",
    );
    require(
        &mut contract_errors,
        !validation_step.contains(".forgejo/workflows/release-prepare.yml")
            && !validation_step.contains(".forgejo/workflows/release-publish.yml")
            && !validation_step.contains("scripts/release")
            && !validation_step.contains(".github/workflows"),
        "release-publish should only allow release metadata paths for token-bearing publish guard: Cargo.toml, Cargo.lock, CHANGELOG.md",
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn release_publish_creates_binary_release_assets_only() {
    let mut contract_errors = Vec::new();
    let Some(job) = workflow_job_in(RELEASE_PUBLISH_WORKFLOW, "release-publish") else {
        panic!(".forgejo/workflows/release-publish.yml should expose a `release-publish` job");
    };

    let Some(release_step) = workflow_step_lines(job, "Create Forgejo Release") else {
        panic!("release-publish should contain a final Create Forgejo Release step");
    };
    let release_step = release_step.join("\n");
    require(
        &mut contract_errors,
        release_step.contains("Linux binary archive")
            && release_step.contains("auto-review-$RELEASE_VERSION-linux-x86_64.tar.gz"),
        "release notes should mention Linux binary archive attachment",
    );
    require(
        &mut contract_errors,
        release_step.contains("--asset release-artifacts/auto-review-$RELEASE_VERSION-linux-x86_64.tar.gz")
            && release_step.contains("--asset release-artifacts/SHA256SUMS")
            && release_step.contains("--asset release-artifacts/SHA256SUMS.sig")
            && release_step.contains("--asset release-artifacts/release-signing-key.pub")
            && release_step.contains("--asset release-artifacts/allowed-signers")
            && release_step.contains("--asset release-artifacts/auto-review-$RELEASE_VERSION-sbom.spdx.json")
            && release_step.contains("--asset release-artifacts/auto-review-$RELEASE_VERSION-provenance.json"),
        "release creation should attach the Linux archive, checksum/signature materials, and SBOM/provenance assets",
    );
    require(
        &mut contract_errors,
        !release_step.contains("docker") && !release_step.contains("digest"),
        "release creation should not include docker image promotion or digest operations",
    );

    assert!(contract_errors.is_empty(), "{}", contract_errors.join("\n"));
}

#[test]
fn release_prepare_creates_mergeable_release_pr_description() {
    let descriptions = release_pr_descriptions(RELEASE_PREPARE_WORKFLOW);

    assert!(
        !descriptions.is_empty()
            && descriptions.iter().all(|description| {
                let lines: Vec<_> = description.lines().collect();
                lines.iter().all(|line| !line.starts_with("    "))
                    && lines.iter().any(|line| {
                        line.contains("CI builds Linux release-candidate tarball artifacts for review")
                            && line.contains("published only after merge to main")
                    })
            }),
        "release-prepare should pass tea release PR descriptions as normal Markdown paragraphs that describe artifact/release behavior, not as four-space-indented code blocks: {descriptions:#?}"
    );
}

fn workflow_job(job_name: &str) -> Option<&'static str> {
    workflow_job_in(CI_WORKFLOW, job_name)
}

fn workflow_job_in(workflow: &'static str, job_name: &str) -> Option<&'static str> {
    let jobs_start = workflow.find("jobs:\n")?;
    let jobs_yaml = &workflow[jobs_start + "jobs:\n".len()..];
    let marker = format!("  {job_name}:");
    let start = jobs_yaml.find(&marker)?;
    let rest = &jobs_yaml[start..];
    let end = rest
        .match_indices('\n')
        .skip(1)
        .find_map(|(index, _)| {
            let line = rest[index + 1..].lines().next().unwrap_or_default();
            is_top_level_workflow_job_key(line).then_some(index)
        })
        .unwrap_or(rest.len());

    Some(&rest[..end])
}

fn workflow_step_lines<'a>(job: &'a str, step_name: &str) -> Option<Vec<&'a str>> {
    let lines: Vec<&'a str> = job.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with("      - name: ") && line.contains(step_name))?;

    let end = (start + 1..lines.len())
        .find(|&i| lines[i].starts_with("      - "))
        .unwrap_or(lines.len());

    Some(lines[start..end].to_vec())
}

fn line_index_with_prefix(lines: &[&str], prefix: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| line.trim().starts_with(prefix))
}

fn shell_case_patterns<'a>(step_lines: &'a [&'a str]) -> Vec<String> {
    let mut in_case = false;
    let mut patterns = Vec::new();

    for line in step_lines {
        let trimmed = line.trim();

        if trimmed.starts_with("case ") && trimmed.ends_with("in") {
            in_case = true;
            continue;
        }

        if !in_case {
            continue;
        }

        if trimmed == "esac" {
            break;
        }

        if trimmed.ends_with(")") {
            patterns.push(trimmed.to_owned());
        }
    }

    patterns
}

fn workflow_jobs(workflow: &'static str) -> Vec<(String, &'static str)> {
    let jobs_start = workflow.find("jobs:\n").unwrap_or(0);
    if jobs_start == 0 {
        return Vec::new();
    }
    let jobs_yaml = &workflow[jobs_start + "jobs:\n".len()..];
    let mut top_level_job_starts: Vec<(String, usize)> = Vec::new();
    let mut cursor = 0usize;

    while cursor < jobs_yaml.len() {
        let line_end = jobs_yaml[cursor..]
            .find('\n')
            .map_or(jobs_yaml.len(), |next| cursor + next);
        let line = &jobs_yaml[cursor..line_end];
        if is_top_level_workflow_job_key(line) {
            top_level_job_starts.push((line.trim().trim_end_matches(':').to_owned(), cursor));
        }
        if line_end == jobs_yaml.len() {
            break;
        }
        cursor = line_end + 1;
    }

    let mut job_sections = Vec::new();
    for (idx, (name, start)) in top_level_job_starts.iter().enumerate() {
        let end = top_level_job_starts
            .get(idx + 1)
            .map(|(_, next_start)| *next_start)
            .unwrap_or(jobs_yaml.len());
        job_sections.push((name.to_owned(), &jobs_yaml[*start..end]));
    }

    job_sections
}

fn workflow_job_output_keys(job: &str) -> Vec<String> {
    let mut in_outputs = false;
    let mut output_keys = Vec::new();

    for line in job.lines() {
        if in_outputs {
            if !line.starts_with("      ") {
                break;
            }

            if let Some((key, _)) = line.trim_start().split_once(':') {
                output_keys.push(key.trim().to_owned());
            }
            continue;
        }

        if line.starts_with("    outputs:") {
            in_outputs = true;
        }
    }

    output_keys
}

fn find_matching_output_key(output_keys: &[String], required: &[&str]) -> Option<String> {
    output_keys.iter().find_map(|key| {
        let lower = key.to_ascii_lowercase();
        if required.iter().all(|needle| lower.contains(needle)) {
            Some(key.to_owned())
        } else {
            None
        }
    })
}

fn workflow_job_if(job: &str) -> Option<String> {
    job.lines().find_map(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("if:") {
            Some(trimmed.to_owned())
        } else {
            None
        }
    })
}

fn workflow_job_needs(job: &str) -> Option<String> {
    job.lines().find_map(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("needs:") {
            Some(trimmed.to_owned())
        } else {
            None
        }
    })
}

fn workflow_job_needs_tokens(needs_line: String) -> Vec<String> {
    let needs_expr = needs_line.trim_start_matches("needs:").trim();
    let needs_expr = needs_expr.trim_start_matches('[').trim_end_matches(']');
    needs_expr
        .split(',')
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn is_top_level_workflow_job_key(line: &str) -> bool {
    line.starts_with("  ")
        && !line.starts_with("    ")
        && line.trim_end().ends_with(':')
        && !line.trim_start().starts_with('#')
}

fn job_contains_run_command(job: &str, expected_command: &str) -> bool {
    job.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed == format!("run: {expected_command}")
            || trimmed.starts_with("run: |") && job.contains(expected_command)
    })
}

fn job_checkout_disables_persisted_credentials(job: &str) -> bool {
    let Some(checkout_start) = job.find("uses: actions/checkout@v4") else {
        return false;
    };
    let checkout_step = job[checkout_start..]
        .split_once("\n      - ")
        .map_or(&job[checkout_start..], |(step, _)| step);

    checkout_step.contains("persist-credentials: false")
}

fn release_pr_descriptions(workflow: &str) -> Vec<String> {
    let script_lines = workflow
        .lines()
        .map(|line| line.strip_prefix("          ").unwrap_or(line));
    let mut descriptions = Vec::new();
    let mut current: Option<String> = None;

    for line in script_lines {
        if let Some(description) = current.as_mut() {
            if let Some((tail, _)) = line.split_once('"') {
                description.push('\n');
                description.push_str(tail);
                descriptions.push(current.take().unwrap_or_default());
            } else {
                description.push('\n');
                description.push_str(line);
            }
            continue;
        }

        if let Some((_, body_start)) = line.split_once("--description \"") {
            if let Some((body, _)) = body_start.split_once('"') {
                descriptions.push(body.to_owned());
            } else {
                current = Some(body_start.to_owned());
            }
        }
    }

    descriptions
}

fn require(errors: &mut Vec<String>, condition: bool, message: impl Into<String>) {
    if !condition {
        errors.push(message.into());
    }
}
