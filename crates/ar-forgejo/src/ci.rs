//! Forgejo CI evidence and guarded review-wait status writes.

#![expect(
    clippy::pub_with_shorthand,
    reason = "lock helpers are shared only with the parent client module"
)]

use super::{
    decode_json_response, json_get, Client, Error, PAGINATION_MAX_PAGES, PAGINATION_PAGE_SIZE,
};
use crate::capabilities::parse_major_version;
use ar_forge::{CommitStatus, CommitStatusState};
use core::array;
use core::future::Future;
use core::hash::{Hash as _, Hasher as _};
use core::time::Duration;
use serde::Deserialize;
use std::collections::HashSet;
use std::hash::DefaultHasher;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::timeout;
use url::Url;

/// The first Forgejo major exposing current job rows for a run.
const RUN_JOBS_MAJOR: u64 = 16;
/// Keep both polling and the status critical section bounded.
pub(super) const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep unrelated writes concurrent without retaining per-revision keys.
const STATUS_WRITE_STRIPES: usize = 32;
/// Select one of the 32 stripes using the five low bits of the hash.
const STATUS_WRITE_MASK: u64 = 31;
/// Review status context shared by watches and workers.
const REVIEW_CONTEXT: &str = "auto_review";
/// Description of a pending wait.
const WAITING: &str = "auto_review waiting for CI";
/// Terminal description identifying a canceled wait.
const CANCELED: &str = "auto_review watch canceled; no review ran";
/// Terminal description identifying an expired wait.
const TIMED_OUT: &str = "auto_review watch timed out; use the re-review command to retry";

/// Fixed-size lock stripes shared across client clones.
#[derive(Debug)]
pub(super) struct StatusWriters([Mutex<()>; STATUS_WRITE_STRIPES]);

impl Default for StatusWriters {
    fn default() -> Self {
        Self(array::from_fn(|_| Mutex::new(())))
    }
}

impl StatusWriters {
    /// Lock the bucket for a repository and commit, ignoring ASCII case.
    pub(super) async fn lock(&self, owner: &str, repo: &str, sha: &str) -> MutexGuard<'_, ()> {
        let mut hash = DefaultHasher::new();
        (
            owner.to_ascii_lowercase(),
            repo.to_ascii_lowercase(),
            sha.to_ascii_lowercase(),
        )
            .hash(&mut hash);
        let bucket = usize::try_from(hash.finish() & STATUS_WRITE_MASK).unwrap_or_default();
        self.0.get(bucket).unwrap_or(&self.0[0]).lock().await
    }
}

/// Page from the repository's Actions runs endpoint.
#[derive(Deserialize)]
struct RunPage {
    /// Result count before pagination.
    total_count: usize,
    /// Runs on this page.
    workflow_runs: Vec<ActionRun>,
}

/// Revision and provenance fields for a workflow run.
#[derive(Deserialize)]
struct ActionRun {
    /// Commit used by this run, independent of the event's current PR head.
    commit_sha: String,
    /// Forgejo hook type.
    event: String,
    /// JSON-encoded hook payload.
    event_payload: String,
    /// Global run id used by the jobs endpoint.
    id: u64,
    /// Display ref, e.g. `#42` for `refs/pull/42/head`.
    prettyref: String,
    /// Workflow `on:` trigger.
    trigger_event: String,
}

/// PR identity stored in the serialized hook payload.
#[derive(Deserialize)]
struct PullPayload {
    /// Repository-local PR number.
    number: u64,
}

impl ActionRun {
    /// Verify this run's PR identity and revision rather than trusting the list filter.
    fn is_for_pr(&self, sha: &str, number: u64, pr_ref: &str) -> Result<bool, Error> {
        // `event` records the Forgejo hook, `trigger_event` the workflow's `on:`.
        // PrettyRef renders `refs/pull/<number>/head` as `#<number>`.
        if self.commit_sha != sha
            || self.prettyref != pr_ref
            || !matches!(
                self.trigger_event.as_str(),
                "pull_request" | "pull_request_target"
            )
            || !matches!(
                self.event.as_str(),
                "pull_request"
                    | "pull_request_sync"
                    | "pull_request_assign"
                    | "pull_request_label"
                    | "pull_request_milestone"
            )
        {
            return Ok(false);
        }
        let payload: PullPayload =
            serde_json::from_str(&self.event_payload).map_err(|error| Error::Api {
                status: 200,
                body: format!(
                    "invalid PR event payload in Actions run {}: {error}",
                    self.id
                ),
            })?;
        Ok(payload.number == number)
    }
}

/// Current job row, not attempt history.
#[derive(Deserialize)]
struct ActionJob {
    /// Exact display name, including any case or matrix suffix.
    name: String,
    /// Forgejo Actions state.
    status: String,
}

/// A status as returned by Forgejo, not the outgoing POST body.
#[derive(Deserialize)]
struct StatusRow {
    /// Status context, compared exactly.
    context: String,
    /// Account that authored the status, when visible.
    creator: Option<StatusCreator>,
    /// Description used to identify a terminal wait.
    description: Option<String>,
    /// Identity allowing a wait to close only its own publication.
    id: u64,
    /// Forgejo names this response field `status`, unlike the POST's `state`.
    status: String,
}

/// Author of a commit status.
#[derive(Deserialize)]
struct StatusCreator {
    /// Forgejo username.
    login: String,
}

/// Status POST response fields needed by the watch.
#[derive(Deserialize)]
struct CreatedStatus {
    /// Missing IDs must never be inferred from subsequent reads.
    id: Option<u64>,
}

/// Current state of a job in a revision-bound Forgejo Actions run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionJobProgress {
    /// The job finished without passing.
    Complete,
    /// The job succeeded or was skipped.
    Passed,
    /// The job has not reached a known terminal state.
    Pending,
}

/// Why a CI watch ended without handing the revision to a review worker.
#[derive(Clone, Copy, Debug)]
pub enum ReviewWaitEnd {
    /// The watch stopped because the PR is no longer eligible.
    Canceled,
    /// The watch ran out of time.
    TimedOut,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "each adapter's public operation and its pagination helper are kept together"
)]
impl Client {
    /// Read current Actions job rows for this PR and exact revision in the base repository.
    /// Keep every occurrence of a job name, including across different workflows and runs.
    /// Older or unidentified servers yield no evidence; commit statuses cannot substitute.
    ///
    /// # Errors
    ///
    /// Returns an error on a failed probe, request, decode, or incomplete scan.
    #[inline]
    pub async fn action_job_progress(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        sha: &str,
    ) -> Result<Vec<(String, ActionJobProgress)>, Error> {
        bounded(STATUS_TIMEOUT, async {
            let version = self.cached_server_version().await?;
            if parse_major_version(version).is_none_or(|major| major < RUN_JOBS_MAJOR) {
                return Ok(Vec::new());
            }
            let path = format!("repos/{owner}/{repo}/actions/runs");
            let ids = self.pr_action_runs(&path, number, sha).await?;
            let mut all = Vec::new();
            for id in ids {
                let url = self.url(&format!("{path}/{id}/jobs"))?;
                // This endpoint returns every current job row as a bare array, not attempts.
                let jobs: Vec<ActionJob> = json_get(&self.http, url).await?;
                all.extend(jobs.into_iter().map(|job| {
                    let progress = match job.status.as_str() {
                        "success" | "skipped" => ActionJobProgress::Passed,
                        "failure" | "cancelled" => ActionJobProgress::Complete,
                        // Includes waiting, running, blocked, and unknown wire values.
                        _ => ActionJobProgress::Pending,
                    };
                    (job.name, progress)
                }));
            }
            Ok(all)
        })
        .await
    }

    /// Enumerate all matching runs before fetching jobs; never return a partial set.
    async fn pr_action_runs(&self, path: &str, number: u64, sha: &str) -> Result<Vec<u64>, Error> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        let mut total = None;
        let pr_ref = format!("#{number}");
        for page in 1..=PAGINATION_MAX_PAGES {
            let mut url = self.url(path)?;
            url.query_pairs_mut()
                .append_pair("head_sha", sha)
                .append_pair("page", &page.to_string())
                .append_pair("limit", &PAGINATION_PAGE_SIZE.to_string());
            let rows: RunPage = json_get(&self.http, url).await?;
            if *total.get_or_insert(rows.total_count) != rows.total_count {
                return Err(incomplete("Actions run"));
            }
            let empty = rows.workflow_runs.is_empty();
            for run in rows.workflow_runs {
                if run.id == 0 || !seen.insert(run.id) {
                    return Err(incomplete("Actions run"));
                }
                if run.is_for_pr(sha, number, &pr_ref)? {
                    ids.push(run.id);
                }
            }
            if seen.len() == rows.total_count {
                return Ok(ids);
            }
            if empty || seen.len() > rows.total_count {
                break;
            }
        }
        Err(incomplete("Actions run"))
    }

    /// Start a wait only if no review status exists or this token's earlier wait ended.
    /// End a wait only if its exact pending status is still the newest review status.
    /// Returns the ID from the created status, not from a later lookup.
    ///
    /// Writers sharing this client (including clones) are serialized. Forgejo does not
    /// provide an atomic conditional write against other processes or external writers.
    ///
    /// # Errors
    ///
    /// Returns an error on an incomplete status lookup, failed request, or invalid response.
    #[inline]
    pub async fn update_review_wait_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        end: Option<(u64, ReviewWaitEnd)>,
    ) -> Result<Option<u64>, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/statuses/{sha}"))?;
        bounded(STATUS_TIMEOUT, async {
            let _guard = self.status_writes.lock(owner, repo, sha).await;
            let latest = self.latest_review_status(&url).await?;
            let (state, description) = match (end, latest) {
                (None, None) => (CommitStatusState::Pending, WAITING),
                (None, Some(row))
                    if row.status == "error"
                        && matches!(row.description.as_deref(), Some(CANCELED | TIMED_OUT)) =>
                {
                    let Some(creator) = row.creator else {
                        return Ok(None);
                    };
                    let login = self
                        .pat_login
                        .get_or_try_init(|| self.get_authenticated_user_login())
                        .await?;
                    if !creator.login.eq_ignore_ascii_case(login) {
                        return Ok(None);
                    }
                    (CommitStatusState::Pending, WAITING)
                }
                (Some((id, reason)), Some(row))
                    if id != 0 && row.id == id && row.status == "pending" =>
                {
                    (
                        CommitStatusState::Error,
                        match reason {
                            ReviewWaitEnd::Canceled => CANCELED,
                            ReviewWaitEnd::TimedOut => TIMED_OUT,
                        },
                    )
                }
                _ => return Ok(None),
            };
            let status = CommitStatus {
                context: REVIEW_CONTEXT.into(),
                description: description.into(),
                state,
                target_url: String::new(),
            };
            let created: CreatedStatus = self
                .post_commit_status_unlocked(url, &status)
                .await?
                .json()
                .await?;
            Ok(created.id.filter(|id| *id != 0))
        })
        .await
    }

    /// Find the newest exact context, or prove its absence after a complete scan.
    async fn latest_review_status(&self, url: &Url) -> Result<Option<StatusRow>, Error> {
        let mut seen = HashSet::new();
        let mut total = None;
        for page in 1..=PAGINATION_MAX_PAGES {
            let mut page_url = url.clone();
            // Forgejo implements `leastindex` as index DESC (newest first).
            page_url
                .query_pairs_mut()
                .append_pair("sort", "leastindex")
                .append_pair("page", &page.to_string())
                .append_pair("limit", &PAGINATION_PAGE_SIZE.to_string());
            let response = self.http.get(page_url).send().await?;
            if let Some(value) = response.headers().get("x-total-count") {
                let Some(count) = value
                    .to_str()
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                else {
                    return Err(incomplete("commit status"));
                };
                if *total.get_or_insert(count) != count {
                    return Err(incomplete("commit status"));
                }
            }
            let rows: Vec<StatusRow> = decode_json_response(response).await?;
            if total.is_some_and(|count| seen.len().saturating_add(rows.len()) > count) {
                return Err(incomplete("commit status"));
            }
            if rows.is_empty() {
                return if total.is_none_or(|count| seen.len() == count) {
                    Ok(None)
                } else {
                    Err(incomplete("commit status"))
                };
            }
            for row in rows {
                if row.id == 0 || !seen.insert(row.id) {
                    return Err(incomplete("commit status"));
                }
                if row.context == REVIEW_CONTEXT {
                    return Ok(Some(row));
                }
            }
            if total == Some(seen.len()) {
                return Ok(None);
            }
            // Without a count, read to an empty page: the host can clamp the requested limit.
        }
        Err(incomplete("commit status"))
    }
}

/// Bound a full operation including any wait for the writer lock.
pub(super) async fn bounded<T>(
    limit: Duration,
    request: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    timeout(limit, request)
        .await
        .unwrap_or(Err(Error::Timeout(limit)))
}

/// The endpoint did not yield a complete or consistent view.
fn incomplete(resource: &str) -> Error {
    Error::Api {
        status: 200,
        body: format!("incomplete Forgejo {resource} pagination"),
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "wire tests propagate setup errors and assert observed results"
)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{body_json, header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use ActionJobProgress::{Complete, Passed, Pending};

    const RUNS: &str = "/forgejo/api/v1/repos/base/repo/actions/runs";
    const STATUSES: &str = "/forgejo/api/v1/repos/base/repo/statuses/head";

    async fn fixture() -> Result<(MockServer, Client), Error> {
        let server = MockServer::start().await;
        let client = Client::new(&format!("{}/forgejo", server.uri()), "token")?;
        Ok((server, client))
    }

    async fn version(server: &MockServer, version: &str) {
        Mock::given(method("GET"))
            .and(path("/forgejo/api/v1/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": version})))
            .expect(1)
            .mount(server)
            .await;
    }

    fn run(id: u64, sha: &str, number: u64) -> Value {
        run_with(id, sha, number, "pull_request_sync", &format!("#{number}"))
    }

    fn run_with(id: u64, sha: &str, number: u64, event: &str, prettyref: &str) -> Value {
        json!({
            "id": id, "commit_sha": sha, "event": event,
            "trigger_event": "pull_request", "prettyref": prettyref,
            "event_payload": json!({"number": number, "pull_request": {"head": {"sha": "different"}}}).to_string(),
        })
    }

    async fn runs(server: &MockServer, page: &str, total: u64, entries: &[Value]) {
        Mock::given(method("GET"))
            .and(path(RUNS))
            .and(header("authorization", "token token"))
            .and(query_param("head_sha", "head"))
            .and(query_param("page", page))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": total, "workflow_runs": entries,
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn action_jobs_require_run_sha_pr_event_ref_and_payload_and_keep_duplicate_names(
    ) -> Result<(), Error> {
        let (server, client) = fixture().await?;
        version(&server, "16.0.0").await;
        let wrong_event = run_with(3, "head", 42, "push", "#42");
        let wrong_ref = run_with(4, "head", 42, "pull_request_sync", "main");
        runs(
            &server,
            "1",
            7,
            &[
                run_with(1, "head", 17, "pull_request_sync", "#42"),
                run(2, "old", 42),
                wrong_event,
                wrong_ref,
            ],
        )
        .await;
        runs(
            &server,
            "2",
            7,
            &[run(5, "head", 42), run(6, "head", 42), run(7, "head", 42)],
        )
        .await;
        let jobs_by_run: [(u64, Value); 3] = [
            (
                5,
                json!([{"name": "Tests", "status": "success"}, {"name": "lint", "status": "skipped"}]),
            ),
            (
                6,
                json!([{"name": "Tests", "status": "waiting"}, {"name": "test", "status": "cancelled"}]),
            ),
            (
                7,
                json!([{"name": "build", "status": "failure"}, {"name": "scan", "status": "unknown"}]),
            ),
        ];
        for (id, jobs) in jobs_by_run {
            Mock::given(method("GET"))
                .and(path(format!("{RUNS}/{id}/jobs")))
                .respond_with(ResponseTemplate::new(200).set_body_json(jobs))
                .expect(1)
                .mount(&server)
                .await;
        }
        let expected = [
            ("Tests", Passed),
            ("lint", Passed),
            ("Tests", Pending),
            ("test", Complete),
            ("build", Complete),
            ("scan", Pending),
        ];
        assert_eq!(
            client
                .action_job_progress("base", "repo", 42, "head")
                .await?,
            expected.map(|(name, progress)| (name.to_owned(), progress))
        );
        Ok(())
    }

    #[tokio::test]
    async fn action_jobs_do_not_return_evidence_after_incomplete_scan() -> Result<(), Error> {
        let (server, client) = fixture().await?;
        version(&server, "17.0.0").await;
        runs(&server, "1", 2, &[run(5, "head", 42)]).await;
        Mock::given(method("GET"))
            .and(path(format!("{RUNS}/5/jobs")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"name": "Tests", "status": "success"}])),
            )
            .mount(&server)
            .await;
        runs(&server, "2", 2, &[]).await;
        assert!(matches!(
            client.action_job_progress("base", "repo", 42, "head").await,
            Err(Error::Api { status: 200, body }) if body == "incomplete Forgejo Actions run pagination"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn action_jobs_do_not_query_runs_or_fall_back_on_forgejo_15() -> Result<(), Error> {
        let (server, client) = fixture().await?;
        version(&server, "15.0.0").await;
        Mock::given(method("GET"))
            .and(path_regex("/repos/"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(
            client
                .action_job_progress("base", "repo", 42, "head")
                .await?,
            Vec::new()
        );
        Ok(())
    }

    fn status_row(id: u64, context: &str, status: &str) -> Value {
        json!({"id": id, "context": context, "status": status})
    }

    async fn status_page(server: &MockServer, page: &str, rows: &[Value]) {
        Mock::given(method("GET"))
            .and(path(STATUSES))
            .and(query_param("sort", "leastindex"))
            .and(query_param("page", page))
            .respond_with(ResponseTemplate::new(200).set_body_json(rows))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn review_wait_captures_created_status_id_and_requires_it_to_still_be_latest(
    ) -> Result<(), Error> {
        let (server, client) = fixture().await?;
        status_page(&server, "1", &[]).await;
        let created_id: u64 = 42;
        Mock::given(method("POST"))
            .and(path(STATUSES))
            .and(body_json(json!({"state": "pending", "description": "auto_review waiting for CI", "context": "auto_review", "target_url": ""})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": created_id})))
            .expect(1)
            .mount(&server)
            .await;
        let id = client
            .update_review_wait_status("base", "repo", "head", None)
            .await?;
        assert_eq!(id, Some(42));
        server.reset().await;

        status_page(&server, "1", &[status_row(702, "other", "success")]).await;
        status_page(
            &server,
            "2",
            &[
                status_row(43, "auto_review", "success"),
                status_row(42, "auto_review", "pending"),
            ],
        )
        .await;
        Mock::given(method("POST"))
            .and(path(STATUSES))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let end = id.map(|wait_id| (wait_id, ReviewWaitEnd::Canceled));
        assert_eq!(
            client
                .update_review_wait_status("base", "repo", "head", end)
                .await?,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn review_wait_closes_the_exact_current_pending_status() -> Result<(), Error> {
        let (server, client) = fixture().await?;
        status_page(&server, "1", &[status_row(42, "auto_review", "pending")]).await;
        let terminal_id: u64 = 43;
        Mock::given(method("POST"))
            .and(path(STATUSES))
            .and(body_json(json!({
                "state": "error",
                "description": "auto_review watch timed out; use the re-review command to retry",
                "context": "auto_review",
                "target_url": ""
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": terminal_id})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client
                .update_review_wait_status(
                    "base",
                    "repo",
                    "head",
                    Some((42, ReviewWaitEnd::TimedOut))
                )
                .await?,
            Some(terminal_id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn review_wait_does_not_write_when_pagination_is_incomplete() -> Result<(), Error> {
        let (server, client) = fixture().await?;
        Mock::given(method("GET"))
            .and(path(STATUSES))
            .and(query_param("sort", "leastindex"))
            .and(query_param("page", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-total-count", "1")
                    .set_body_string("[]"),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(STATUSES))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert!(matches!(
            client
                .update_review_wait_status("base", "repo", "head", None)
                .await,
            Err(Error::Api { status: 200, body }) if body == "incomplete Forgejo commit status pagination"
        ));
        Ok(())
    }
}
