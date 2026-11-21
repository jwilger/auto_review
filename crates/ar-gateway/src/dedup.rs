//! Bounded LRU set of recently-seen webhook delivery IDs.
//!
//! Forgejo emits a unique `X-Forgejo-Delivery` UUID per webhook
//! delivery; if a downstream blip causes Forgejo to retry, we get
//! the same UUID twice. Without dedup at this layer the
//! orchestrator's `last_reviewed_sha` history check eventually
//! catches duplicates — but only after the first job either
//! finished or failed, leaving a window where two reviews run in
//! parallel against the same SHA. Cheap to fix with an in-memory
//! LRU.
//!
//! Capacity-bounded: the set holds at most `capacity` IDs;
//! oldest are evicted as new ones arrive. Default 256 covers
//! thousands of seconds of typical traffic on a single-tenant
//! deploy.

use alloc::collections::VecDeque;
use async_trait::async_trait;
use core::str::FromStr as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

/// Schema for the persistent dedup store.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id  TEXT PRIMARY KEY
);
";

/// Recently observed IDs and their eviction order.
#[derive(Debug, Default)]
struct DedupState {
    /// IDs already recorded.
    ids: HashSet<String>,
    /// Insertion order, oldest first.
    order: VecDeque<String>,
}

#[derive(Debug)]
pub struct RecentDeliveries {
    /// Maximum number of retained IDs.
    capacity: usize,
    /// IDs and order, updated under one lock.
    state: Mutex<DedupState>,
}

impl RecentDeliveries {
    /// Check whether `id` is a duplicate of a recently-seen
    /// delivery. Returns true on first sight (caller should
    /// proceed); false on duplicate (caller should reply 200 OK
    /// without further processing).
    ///
    /// Inserts `id` into the set on first sight; evicts the
    /// oldest entry when capacity is exceeded.
    ///
    /// # Panics
    ///
    /// Panics if a previous caller poisoned the dedup lock.
    #[inline]
    #[expect(clippy::expect_used, reason = "preserve the poisoned-lock policy")]
    #[expect(
        clippy::same_name_method,
        reason = "keep the synchronous API alongside its async trait adapter"
    )]
    pub fn check_and_record(&self, id: &str) -> CheckResult {
        let mut state = self.state.lock().expect("dedup lock");
        if state.ids.contains(id) {
            return CheckResult::Duplicate;
        }
        if state.order.len() >= self.capacity {
            if let Some(old) = state.order.pop_front() {
                state.ids.remove(&old);
            }
        }
        state.ids.insert(id.to_owned());
        state.order.push_back(id.to_owned());
        CheckResult::FirstSight
    }

    /// Construct an empty set with at least one slot.
    #[inline]
    #[must_use = "this constructor returns an empty dedup store"]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(DedupState::default()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "preserve public enum discriminants"
)]
pub enum CheckResult {
    FirstSight,
    Duplicate,
}

#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::module_name_repetitions,
    reason = "keep the public error type name"
)]
pub enum DedupError {
    #[error("storage: {0}")]
    Storage(String),
}

/// Common interface across the in-memory and SQLite dedup backings,
/// so `AppState.webhook_dedup` can hold either via `Arc<dyn _>`.
/// The async signature accommodates the SQLite impl's pool round-trip;
/// the in-memory impl returns immediately under a `Mutex`.
#[async_trait]
#[expect(clippy::module_name_repetitions, reason = "keep the public trait name")]
pub trait DeliveryDedup: Send + Sync {
    async fn check_and_record(&self, id: &str) -> Result<CheckResult, DedupError>;
}

#[async_trait]
impl DeliveryDedup for RecentDeliveries {
    #[inline]
    async fn check_and_record(&self, id: &str) -> Result<CheckResult, DedupError> {
        Ok(RecentDeliveries::check_and_record(self, id))
    }
}

#[async_trait]
impl DeliveryDedup for SqliteDeliveries {
    #[inline]
    async fn check_and_record(&self, id: &str) -> Result<CheckResult, DedupError> {
        SqliteDeliveries::check_and_record(self, id).await
    }
}

/// SQLite-backed delivery dedup. Survives gateway restarts: a
/// Forgejo redelivery that lands while the gateway was bouncing
/// is still recognised as a duplicate when the bot comes back up.
pub struct SqliteDeliveries {
    /// Connections to the persistent backing store.
    pool: Pool<Sqlite>,
}

impl SqliteDeliveries {
    /// Atomically record the delivery ID and report whether it was seen before.
    ///
    /// # Errors
    ///
    /// Returns [`DedupError::Storage`] if the insert fails.
    #[inline]
    #[expect(
        clippy::same_name_method,
        reason = "keep the inherent API alongside its trait adapter"
    )]
    pub async fn check_and_record(&self, id: &str) -> Result<CheckResult, DedupError> {
        // INSERT OR IGNORE returns 0 affected rows when the PK
        // already exists, 1 on first sight. Single-statement
        // atomicity sidesteps the read-then-write race the
        // in-memory Mutex guards against.
        let res = sqlx::query("INSERT OR IGNORE INTO webhook_deliveries(id) VALUES (?)")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|error| DedupError::Storage(error.to_string()))?;
        if res.rows_affected() == 0 {
            Ok(CheckResult::Duplicate)
        } else {
            Ok(CheckResult::FirstSight)
        }
    }

    /// Open or create the dedup database and initialize its schema.
    ///
    /// # Errors
    ///
    /// Returns [`DedupError::Storage`] if the path cannot be parsed, the
    /// database cannot be opened, or schema initialization fails.
    #[inline]
    pub async fn open(path: &Path) -> Result<Self, DedupError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|error| DedupError::Storage(error.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|error| DedupError::Storage(error.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|error| DedupError::Storage(error.to_string()))?;
        Ok(Self { pool })
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test database and fixture operations must succeed"
)]
mod tests {
    use super::*;
    use alloc::sync::Arc;

    #[test]
    fn first_id_is_first_sight() {
        let dedup = RecentDeliveries::new(8);
        assert_eq!(dedup.check_and_record("abc"), CheckResult::FirstSight);
    }

    #[test]
    fn second_check_of_same_id_is_duplicate() {
        let dedup = RecentDeliveries::new(8);
        dedup.check_and_record("abc");
        assert_eq!(dedup.check_and_record("abc"), CheckResult::Duplicate);
        // And again — same answer.
        assert_eq!(dedup.check_and_record("abc"), CheckResult::Duplicate);
    }

    #[test]
    fn capacity_evicts_oldest_first() {
        let dedup = RecentDeliveries::new(3);
        for id in ["a", "b", "c"] {
            assert_eq!(dedup.check_and_record(id), CheckResult::FirstSight);
        }
        // Set holds {a, b, c} in insertion order. Adding "d"
        // evicts "a" (the oldest).
        assert_eq!(dedup.check_and_record("d"), CheckResult::FirstSight);
        // "a" was evicted, so re-checking sees first-sight.
        assert_eq!(
            dedup.check_and_record("a"),
            CheckResult::FirstSight,
            "evicted entry should appear as first-sight again"
        );
        // After {b, c, d, a} → {c, d, a} (b just evicted).
        // "c" and "d" should still be duplicates.
        assert_eq!(dedup.check_and_record("c"), CheckResult::Duplicate);
        assert_eq!(dedup.check_and_record("d"), CheckResult::Duplicate);
    }

    #[test]
    fn zero_capacity_clamps_to_one() {
        // Defensive: zero would always evict on insert, never
        // remembering anything. Clamp prevents that footgun.
        let dedup = RecentDeliveries::new(0);
        assert_eq!(dedup.check_and_record("x"), CheckResult::FirstSight);
        assert_eq!(dedup.check_and_record("x"), CheckResult::Duplicate);
    }

    #[tokio::test]
    async fn sqlite_dedup_first_sight_returns_firstsight() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.db");
        let dedup = SqliteDeliveries::open(&path).await.expect("open");
        let result = dedup.check_and_record("uuid-1").await.expect("record");
        assert_eq!(result, CheckResult::FirstSight);
    }

    #[tokio::test]
    async fn sqlite_dedup_second_check_of_same_id_returns_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.db");
        let dedup = SqliteDeliveries::open(&path).await.expect("open");
        dedup.check_and_record("uuid-1").await.expect("first");
        let second = dedup.check_and_record("uuid-1").await.expect("second");
        assert_eq!(second, CheckResult::Duplicate);
    }

    #[tokio::test]
    async fn delivery_dedup_trait_object_dispatches_to_in_memory() {
        let dedup: Arc<dyn DeliveryDedup> = Arc::new(RecentDeliveries::new(8));
        assert_eq!(
            dedup.check_and_record("uuid-1").await.expect("record"),
            CheckResult::FirstSight,
        );
        assert_eq!(
            dedup.check_and_record("uuid-1").await.expect("record"),
            CheckResult::Duplicate,
        );
    }

    #[tokio::test]
    async fn delivery_dedup_trait_object_dispatches_to_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.db");
        let dedup: Arc<dyn DeliveryDedup> =
            Arc::new(SqliteDeliveries::open(&path).await.expect("open"));
        assert_eq!(
            dedup.check_and_record("uuid-1").await.expect("record"),
            CheckResult::FirstSight,
        );
        assert_eq!(
            dedup.check_and_record("uuid-1").await.expect("record"),
            CheckResult::Duplicate,
        );
    }

    #[tokio::test]
    async fn sqlite_dedup_persists_across_reopen() {
        // The whole point of the SQLite backing: after a gateway
        // restart, an in-flight Forgejo retry is still recognised.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.db");
        let first = SqliteDeliveries::open(&path).await.expect("open");
        first.check_and_record("uuid-1").await.expect("record");
        drop(first);
        let reopened = SqliteDeliveries::open(&path).await.expect("reopen");
        let after_restart = reopened.check_and_record("uuid-1").await.expect("re-check");
        assert_eq!(after_restart, CheckResult::Duplicate);
    }

    #[test]
    fn distinct_ids_dont_clash() {
        let dedup = RecentDeliveries::new(8);
        for id in ["a", "b", "c", "d"] {
            assert_eq!(dedup.check_and_record(id), CheckResult::FirstSight);
        }
        for id in ["a", "b", "c", "d"] {
            assert_eq!(dedup.check_and_record(id), CheckResult::Duplicate);
        }
    }
}
