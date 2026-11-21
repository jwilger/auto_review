#![expect(
    clippy::tests_outside_test_module,
    reason = "this integration target is already a test harness"
)]

extern crate alloc;

use alloc::sync::Arc;
use ar_gateway::poller::{claim_chat_comment, SharedCommentCursors};
use ar_orchestrator::review_history::PrKey;
use std::collections::HashMap;
use tokio::sync::Mutex;

fn pr_key(owner: &str, repo: &str, pr_number: u64) -> PrKey {
    PrKey {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        pr_number,
    }
}

#[tokio::test]
async fn shared_chat_comment_claims_deduplicate_by_pr_and_comment_id() {
    let cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::new()));
    let widgets = pr_key("alice", "widgets", 1);
    let gadgets = pr_key("alice", "gadgets", 1);

    assert!(claim_chat_comment(&cursors, widgets.clone(), 9).await);
    assert!(!claim_chat_comment(&cursors, widgets.clone(), 9).await);
    assert!(!claim_chat_comment(&cursors, widgets.clone(), 8).await);

    assert!(claim_chat_comment(&cursors, gadgets, 9).await);
    assert!(claim_chat_comment(&cursors, widgets, 10).await);
}
