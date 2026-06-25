use ar_github::Client;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn github_clone_url_uses_x_access_token_userinfo_and_encodes_token() {
    let url = ar_github::build_clone_url(
        "https://github.example.com",
        "owner",
        "repo",
        "ghs_to/k:n#1",
    )
    .expect("clone url");

    assert_eq!(
        url,
        "https://x-access-token:ghs_to%2Fk%3An%231@github.example.com/owner/repo.git"
    );
}

#[tokio::test]
async fn get_pull_request_uses_installation_token_and_maps_summary() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "fix: use host neutral PR DTO",
            "body": null,
            "draft": true,
            "state": "open",
            "head": {
                "ref": "feature",
                "sha": "head-sha"
            },
            "base": {
                "ref": "main",
                "sha": "base-sha"
            }
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let pr = client
        .get_pull_request("owner", "repo", 7, "installation-token")
        .await
        .expect("pull request");

    assert_eq!(pr.number, 7);
    assert_eq!(pr.title, "fix: use host neutral PR DTO");
    assert_eq!(pr.body, "");
    assert!(pr.draft);
    assert_eq!(pr.state, "open");
    assert_eq!(pr.head.ref_name, "feature");
    assert_eq!(pr.head.sha, "head-sha");
    assert_eq!(pr.base.ref_name, "main");
    assert_eq!(pr.base.sha, "base-sha");
}

#[tokio::test]
async fn review_host_get_pull_request_uses_installation_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "fix: host PR fetch",
            "body": "details",
            "draft": false,
            "state": "open",
            "head": {
                "ref": "feature",
                "sha": "head-sha"
            },
            "base": {
                "ref": "main",
                "sha": "base-sha"
            }
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let pr = host
        .get_pull_request("owner", "repo", 7)
        .await
        .expect("pull request");

    assert_eq!(pr.title, "fix: host PR fetch");
    assert_eq!(pr.head.sha, "head-sha");
}

#[tokio::test]
async fn review_host_clone_url_uses_installation_token_credentials() {
    let client = Client::new("https://github.example.com", "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "ghs_token");
    let host: &dyn ar_forge::ReviewHost = &host;

    assert_eq!(
        host.clone_url("owner", "repo").await.expect("clone url"),
        "https://x-access-token:ghs_token@github.example.com/owner/repo.git"
    );
}

#[tokio::test]
async fn list_changed_files_uses_installation_token_and_paginates() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/files"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut page = Vec::new();
            page.push(serde_json::json!({
                "filename": "src/lib.rs",
                "status": "modified",
                "additions": 3,
                "deletions": 1,
                "changes": 4,
                "patch": "@@ -1 +1 @@\n-old\n+new"
            }));
            for n in 1..100 {
                page.push(serde_json::json!({
                    "filename": format!("generated/{n}.txt"),
                    "status": "modified",
                    "additions": 1,
                    "deletions": 0,
                    "changes": 1
                }));
            }
            page
        }))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/files"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "filename": "README.md",
                "status": "added",
                "additions": 5,
                "deletions": 0,
                "changes": 5
            }
        ])))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let files = client
        .list_changed_files("owner", "repo", 7, "installation-token")
        .await
        .expect("changed files");

    assert_eq!(files.len(), 101);
    assert_eq!(files[0].filename, "src/lib.rs");
    assert_eq!(files[0].status, "modified");
    assert_eq!(files[0].additions, 3);
    assert_eq!(files[0].deletions, 1);
    assert_eq!(files[0].changes, 4);
    assert_eq!(files[0].patch.as_deref(), Some("@@ -1 +1 @@\n-old\n+new"));
    assert_eq!(files[100].filename, "README.md");
    assert_eq!(files[100].status, "added");
    assert_eq!(files[100].patch, None);
}

#[tokio::test]
async fn get_pull_request_diff_uses_installation_token_and_diff_media_type() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github.v3.diff"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "diff --git a/src/lib.rs b/src/lib.rs\n\
             index 1111111..2222222 100644\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n",
        ))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let diff = client
        .get_pull_request_diff("owner", "repo", 7, "installation-token")
        .await
        .expect("pull request diff");

    assert!(diff.starts_with("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(diff.contains("+new"));
}

#[tokio::test]
async fn review_host_get_pr_diff_dispatches_through_installation_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github.v3.diff"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("diff --git a/src/lib.rs b/src/lib.rs\n+new\n"),
        )
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let diff = host
        .get_pr_diff("owner", "repo", 7)
        .await
        .expect("trait diff");

    assert!(diff.contains("+new"));
}

#[tokio::test]
async fn review_host_list_pull_reviews_uses_installation_token_and_paginates() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/reviews"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut page = Vec::new();
            page.push(serde_json::json!({
                "id": 2001,
                "state": "COMMENTED",
                "user": {"login": "alice"}
            }));
            for n in 2..=100 {
                page.push(serde_json::json!({
                    "id": 2000 + n,
                    "state": "APPROVED",
                    "user": {"login": "reviewer"}
                }));
            }
            page
        }))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/reviews"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 2101,
                "state": "CHANGES_REQUESTED",
                "user": {"login": "bob"}
            }
        ])))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let reviews = host
        .list_pull_reviews("owner", "repo", 7)
        .await
        .expect("reviews");

    assert_eq!(reviews.len(), 101);
    assert_eq!(reviews[0].id, 2001);
    assert_eq!(reviews[0].state, "COMMENTED");
    assert_eq!(reviews[0].user.login, "alice");
    assert_eq!(reviews[100].id, 2101);
    assert_eq!(reviews[100].state, "CHANGES_REQUESTED");
    assert_eq!(reviews[100].user.login, "bob");
}

#[tokio::test]
async fn review_host_list_pull_review_comments_uses_installation_token_and_paginates() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/reviews/2001/comments"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut page = Vec::new();
            page.push(serde_json::json!({
                "id": 3001,
                "body": "Please adjust this line.",
                "user": {"login": "alice"}
            }));
            for n in 2..=100 {
                page.push(serde_json::json!({
                    "id": 3000 + n,
                    "body": format!("inline comment {n}"),
                    "user": {"login": "reviewer"}
                }));
            }
            page
        }))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7/reviews/2001/comments"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 3101,
                "body": "follow-up inline",
                "user": {"login": "bob"}
            }
        ])))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let comments = host
        .list_pull_review_comments("owner", "repo", 7, 2001)
        .await
        .expect("review comments");

    assert_eq!(comments.len(), 101);
    assert_eq!(comments[0].id, 3001);
    assert_eq!(comments[0].body, "Please adjust this line.");
    assert_eq!(comments[0].user.login, "alice");
    assert_eq!(comments[100].id, 3101);
    assert_eq!(comments[100].body, "follow-up inline");
    assert_eq!(comments[100].user.login, "bob");
}

#[tokio::test]
async fn review_host_update_pull_request_uses_installation_token_and_omits_unset_fields() {
    let github = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(body_json(serde_json::json!({
            "body": "Updated body with override marker."
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "unchanged",
            "body": "Updated body with override marker.",
            "head": {"ref": "feature", "sha": "head-sha"},
            "base": {"ref": "main", "sha": "base-sha"}
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    host.update_pull_request(
        "owner",
        "repo",
        7,
        None,
        Some("Updated body with override marker."),
    )
    .await
    .expect("updated pull request");
}

#[tokio::test]
async fn create_review_uses_installation_token_and_maps_inline_comment_position() {
    let github = MockServer::start().await;
    let request = ar_forge::CreateReviewRequest {
        body: "LGTM with notes".to_string(),
        commit_id: "head-sha".to_string(),
        event: ar_forge::ReviewEvent::RequestChanges,
        comments: vec![ar_forge::ReviewComment {
            path: "src/lib.rs".to_string(),
            body: "This misses the edge case.".to_string(),
            old_position: None,
            new_position: Some(42),
        }],
    };
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/7/reviews"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(body_json(serde_json::json!({
            "body": "LGTM with notes",
            "commit_id": "head-sha",
            "event": "REQUEST_CHANGES",
            "comments": [
                {
                    "path": "src/lib.rs",
                    "position": 42,
                    "body": "This misses the edge case."
                }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 99,
            "state": "CHANGES_REQUESTED"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let created = client
        .create_review("owner", "repo", 7, &request, "installation-token")
        .await
        .expect("created review");

    assert_eq!(created.id, 99);
    assert_eq!(created.state, "CHANGES_REQUESTED");
}

#[tokio::test]
async fn post_commit_status_uses_installation_token_and_shared_status_body() {
    let github = MockServer::start().await;
    let status = ar_forge::CommitStatus {
        state: ar_forge::CommitStatusState::Success,
        target_url: "https://ci.example.test/runs/1".to_string(),
        description: "semantic review passed".to_string(),
        context: "auto-review/semantic".to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/statuses/head-sha"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(body_json(serde_json::json!({
            "state": "success",
            "target_url": "https://ci.example.test/runs/1",
            "description": "semantic review passed",
            "context": "auto-review/semantic"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 123,
            "state": "success"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    client
        .post_commit_status("owner", "repo", "head-sha", &status, "installation-token")
        .await
        .expect("commit status");
}

#[tokio::test]
async fn review_host_post_commit_status_uses_installation_token() {
    let github = MockServer::start().await;
    let status = ar_forge::CommitStatus {
        state: ar_forge::CommitStatusState::Pending,
        target_url: String::new(),
        description: "auto_review running".to_string(),
        context: "auto_review".to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/statuses/head-sha"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(body_json(serde_json::json!({
            "state": "pending",
            "target_url": "",
            "description": "auto_review running",
            "context": "auto_review"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 456,
            "state": "pending"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    host.post_commit_status("owner", "repo", "head-sha", &status)
        .await
        .expect("commit status");
}

#[tokio::test]
async fn review_host_get_compare_diff_uses_installation_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/compare/base-sha...head-sha"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github.v3.diff"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("diff --git a/src/lib.rs b/src/lib.rs\n+new line\n"),
        )
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = ar_github::InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let diff = host
        .get_compare_diff("owner", "repo", "base-sha", "head-sha")
        .await
        .expect("compare diff");

    assert_eq!(diff, "diff --git a/src/lib.rs b/src/lib.rs\n+new line\n");
}

#[tokio::test]
async fn list_pr_review_comments_uses_issue_comments_endpoint_and_paginates() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut page = Vec::new();
            page.push(serde_json::json!({
                "id": 1001,
                "body": "@auto-review help",
                "user": {"login": "alice"}
            }));
            for n in 2..=100 {
                page.push(serde_json::json!({
                    "id": 1000 + n,
                    "body": format!("comment {n}"),
                    "user": {"login": "reviewer"}
                }));
            }
            page
        }))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 1101,
                "body": "follow-up",
                "user": {"login": "bob"}
            }
        ])))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let comments = client
        .list_pr_review_comments("owner", "repo", 7, "installation-token")
        .await
        .expect("comments");

    assert_eq!(comments.len(), 101);
    assert_eq!(comments[0].id, 1001);
    assert_eq!(comments[0].body, "@auto-review help");
    assert_eq!(comments[0].user.login, "alice");
    assert_eq!(comments[100].id, 1101);
    assert_eq!(comments[100].body, "follow-up");
    assert_eq!(comments[100].user.login, "bob");
}

#[tokio::test]
async fn post_issue_comment_uses_installation_token_and_body_payload() {
    let github = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(body_json(serde_json::json!({
            "body": "Queued a fresh semantic review."
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 1201,
            "body": "Queued a fresh semantic review.",
            "user": {"login": "auto-review[bot]"}
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let id = client
        .post_issue_comment(
            "owner",
            "repo",
            7,
            "Queued a fresh semantic review.",
            "installation-token",
        )
        .await
        .expect("comment id");

    assert_eq!(id, 1201);
}

#[tokio::test]
async fn get_file_content_uses_contents_raw_media_type_and_ref() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/contents/.auto_review.yaml"))
        .and(query_param("ref", "head-sha"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github.raw+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("enabled: true\nguidelines:\n  - Prefer explicit errors.\n"),
        )
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let contents = client
        .get_file_content(
            "owner",
            "repo",
            ".auto_review.yaml",
            "head-sha",
            "installation-token",
        )
        .await
        .expect("file content");

    assert_eq!(
        contents.as_deref(),
        Some("enabled: true\nguidelines:\n  - Prefer explicit errors.\n")
    );
}

#[tokio::test]
async fn get_file_content_returns_none_when_github_returns_404() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/contents/.auto_review.yaml"))
        .and(query_param("ref", "head-sha"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github.raw+json"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "message": "Not Found"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let contents = client
        .get_file_content(
            "owner",
            "repo",
            ".auto_review.yaml",
            "head-sha",
            "installation-token",
        )
        .await
        .expect("missing file should not be an error");

    assert_eq!(contents, None);
}
