//! Forgejo REST client (Gitea-compatible API).
//!
//! Endpoints used by the reviewer:
//! - `GET /repos/{owner}/{repo}/pulls/{n}.diff` → unified diff
//! - `GET /repos/{owner}/{repo}/pulls/{n}/files` → changed-files list
//! - `POST /repos/{owner}/{repo}/pulls/{n}/reviews` → post review with inline comments
//! - `POST /repos/{owner}/{repo}/statuses/{sha}` → commit status

extern crate alloc;

pub mod capabilities;
pub mod client;
pub mod init;
pub mod types;
pub mod webhook;

use core::future::Future;
use core::time::Duration;
use tokio::time::timeout;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-cli (commands), ar-gateway (startup, webhook, poller), ar-chat, ar-orchestrator \
              and ar-review import these items from the crate root; dropping the re-exports \
              breaks them"
)]
pub use crate::{
    client::{Client, Error},
    init::InitClient,
    types::{
        ChangedFile, CommitStatus, CommitStatusState, CreateAccessTokenRequest,
        CreateReviewRequest, CreateWebhookRequest, CreatedAccessToken, CreatedWebhook,
        PrReviewComment, PrReviewCommentUser, PullRequestRefSummary, PullRequestSummary,
        PullReviewSummary, ReviewComment, ReviewEvent, WebhookConfig, WebhookSummary,
    },
    webhook::{
        Comment, Issue, IssueCommentAction, IssueCommentEvent, IssuePullRequestRef,
        PullRequestAction, PullRequestEvent,
    },
};

/// Upper bound on a host-identity request (`GET /version`, `GET /user`).
/// Concurrent callers wait on the same cached initialiser, so a server that
/// accepts the connection but never answers must not block them forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap the response body included in error messages. Forgejo
/// errors are usually small JSON ({"message": "..."}), but a
/// misbehaving reverse proxy can return a multi-KB HTML stub.
/// Including it verbatim in `Error::Api` holds that much in memory
/// and pollutes operator logs. 1 KiB easily covers any real
/// Forgejo error response while bounding the worst case.
fn cap_for_error(body: &str) -> String {
    /// Largest body prefix, in bytes, kept in an error message.
    const CAP: usize = 1024;
    if body.len() <= CAP {
        return body.to_owned();
    }
    let mut cut = CAP;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let kept = body.get(..cut).unwrap_or_default();
    format!("{kept}… [+{} bytes]", body.len().saturating_sub(cut))
}

/// Run a host-identity request under [`PROBE_TIMEOUT`], turning an
/// expired deadline into [`Error::Timeout`].
async fn probe<T>(request: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    timeout(PROBE_TIMEOUT, request)
        .await
        .unwrap_or(Err(Error::Timeout(PROBE_TIMEOUT)))
}

/// Convert a Forgejo client error into the provider-neutral host error,
/// keeping its `Display` text as the message.
fn host_error(error: &Error) -> ar_forge::HostError {
    ar_forge::HostError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::future;

    #[tokio::test(start_paused = true)]
    async fn probe_times_out_a_request_that_never_answers() {
        let result = probe(future::pending::<Result<(), Error>>()).await;

        assert!(matches!(result, Err(Error::Timeout(_))), "{result:?}");
    }
}
