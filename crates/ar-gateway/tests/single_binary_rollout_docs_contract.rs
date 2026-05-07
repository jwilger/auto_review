const OPERATIONS: &str = include_str!("../../../docs/OPERATIONS.md");
const SYSTEMD_README: &str = include_str!("../../../deploy/systemd/README.md");
const SYSTEMD_ENV: &str = include_str!("../../../deploy/systemd/auto_review.env.example");
const THREAT_MODEL: &str = include_str!("../../../docs/THREAT-MODEL.md");
const ADR_0002: &str = include_str!("../../../docs/ADR-0002-sandbox.md");
const RELEASE_ANNOUNCEMENT: &str = include_str!("../../../docs/RELEASE_ANNOUNCEMENT.md");

#[test]
fn single_binary_oci_rollout_docs_are_honest_about_deployment_and_supply_chain() {
    let mut errors = Vec::new();

    let operator_docs = format!("{OPERATIONS}\n{SYSTEMD_README}\n{SYSTEMD_ENV}");
    let lower_operator_docs = operator_docs.to_lowercase();
    require(
        &mut errors,
        contains_all(
            &lower_operator_docs,
            &["docker", "recommended", "production", "direct", "binary"],
        ),
        "operator docs should distinguish recommended Docker production deployment from direct binary use",
    );
    require(
        &mut errors,
        contains_all(
            &lower_operator_docs,
            &["auto-review gateway", "embedded oci", "default", "ar_gateway_bare", "opt-out"],
        ),
        "operator docs should say direct binary gateway startup uses embedded OCI by default and AR_GATEWAY_BARE is an explicit opt-out",
    );

    let systemd_docs = format!("{SYSTEMD_README}\n{SYSTEMD_ENV}").to_lowercase();
    require(
        &mut errors,
        contains_all(&systemd_docs, &["ar_gateway_bare=true", "systemd"]),
        "systemd docs/env should show that direct systemd deployment sets AR_GATEWAY_BARE=true",
    );
    require(
        &mut errors,
        contains_all(
            &systemd_docs,
            &[
                "application-level controls",
                "systemd hardening",
                "not container-equivalent",
            ],
        ),
        "systemd docs/env should state bare mode has only app-level controls plus systemd hardening, not container-equivalent isolation",
    );

    let threat_model = THREAT_MODEL.to_lowercase();
    require(
        &mut errors,
        contains_all(&threat_model, &["release", "binary", "asset", "provenance"]),
        "threat model should cover release binary assets/provenance",
    );
    require(
        &mut errors,
        contains_all(
            &threat_model,
            &["release publishing pat", "blast radius", "forgejo releases"],
        ),
        "threat model should cover the release-publish PAT blast radius",
    );

    let docs_that_must_not_preclaim_issue_121 =
        format!("{OPERATIONS}\n{SYSTEMD_README}\n{THREAT_MODEL}\n{RELEASE_ANNOUNCEMENT}")
            .to_lowercase();
    for forbidden in [
        "downloadable binaries are published",
        "downloadable binaries are available",
        "binary release artifacts are published",
        "can publish container images\nto git.johnwilger.com/jwilger/auto_review/ar-gateway, including release candidate tags, attach release artifacts",
        "can publish container images to git.johnwilger.com/jwilger/auto_review/ar-gateway, including release candidate tags, attach release artifacts",
        "issue #121 is complete",
        "#121 is complete",
    ] {
        require(
            &mut errors,
            !docs_that_must_not_preclaim_issue_121.contains(forbidden),
            format!("docs should not claim issue #121 binary artifacts already exist: `{forbidden}`"),
        );
    }

    let adr_0002 = ADR_0002.to_lowercase();
    require(
        &mut errors,
        contains_all(
            &adr_0002,
            &["adr-0006", "embedded oci", "gateway isolation"],
        ),
        "ADR-0002 should point readers to ADR-0006 for the embedded OCI gateway isolation decision",
    );

    let release_announcement = RELEASE_ANNOUNCEMENT.to_lowercase();
    require(
        &mut errors,
        contains_all(&release_announcement, &["single", "auto-review", "cli"]),
        "release announcement copy should mention the shipped single `auto-review` CLI",
    );
    require(
        &mut errors,
        contains_all(
            &release_announcement,
            &["container", "first", "production", "deployment"],
        ),
        "release announcement copy should mention container-first production deployment",
    );
    if release_announcement.contains("downloadable binaries") {
        require(
            &mut errors,
            contains_all(
                &release_announcement,
                &["downloadable binaries", "#121", "release artifacts", "provenance"],
            ),
            "release announcement copy may mention downloadable binaries only as #121 release artifacts/provenance scope",
        );
    }

    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

fn contains_all(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| haystack.contains(needle))
}

fn require(errors: &mut Vec<String>, condition: bool, message: impl Into<String>) {
    if !condition {
        errors.push(message.into());
    }
}
