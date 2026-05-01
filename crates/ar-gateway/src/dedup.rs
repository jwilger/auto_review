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

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

#[derive(Debug)]
pub struct RecentDeliveries {
    capacity: usize,
    state: Mutex<DedupState>,
}

#[derive(Debug, Default)]
struct DedupState {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl RecentDeliveries {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(DedupState::default()),
        }
    }

    /// Check whether `id` is a duplicate of a recently-seen
    /// delivery. Returns true on first sight (caller should
    /// proceed); false on duplicate (caller should reply 200 OK
    /// without further processing).
    ///
    /// Inserts `id` into the set on first sight; evicts the
    /// oldest entry when capacity is exceeded.
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
        state.ids.insert(id.to_string());
        state.order.push_back(id.to_string());
        CheckResult::FirstSight
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    FirstSight,
    Duplicate,
}

#[cfg(test)]
mod tests {
    use super::*;

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
