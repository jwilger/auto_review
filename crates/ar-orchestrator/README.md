# ar-orchestrator

Per-PR job lifecycle. Owns the `JobDispatcher` trait, the production
`SpawningDispatcher`, the `ReviewHistory` trait + impls, and the
`ReviewObserver` trait that surfaces lifecycle events to the
gateway's metrics layer.

## Public surface

| Module | What's in it |
|--------|-------------|
| `dispatcher::JobDispatcher` | Trait the gateway calls. `NoOpDispatcher` for tests; `SpawningDispatcher` for production. |
| `dispatcher::ReviewObservation`, `ReviewObserver` | Lifecycle-event trait the gateway implements via `MetricsObserver`. Keeps the orchestrator metrics-format-agnostic. |
| `dispatcher::run_review_job` | The end-to-end review pipeline activity: clone, RAG-context, review, verify, post. Used by both the dispatcher and the `auto_review review-once` CLI command. |
| `review_history::ReviewHistory` | Trait for per-PR "last reviewed SHA" tracking. `InMemoryReviewHistory` for ephemeral; `SqliteReviewHistory` for persistent. |
| `state` | State-machine enum + transitions for the per-PR `pr_run` row. |

## Architecture

The dispatcher fire-and-forgets via `tokio::spawn`. Reviews run in
the background; the gateway returns 202 ACCEPTED to Forgejo
immediately. Spawned tasks are joined via an inner `tokio::spawn`
+ outer `await` so panics surface as a "crashed" commit-status
post on the PR rather than a silent failure.

Review observability flows through `ReviewObserver`:

```
run_review_job → emits ReviewObservation::{Started,
  Succeeded{duration, findings_count},
  Failed{duration, error_class},
  Skipped{reason}}
                ↓
       gateway::metrics::MetricsObserver
                ↓
       AtomicU64 counters → /metrics
```

## Tests

`cargo test -p ar-orchestrator` covers the in-memory and SQLite
history impls, the dispatcher's spawn-and-track lifecycle, and the
review-job activity's commit-status posting on success / failure.
8 of those tests sit on `SqliteReviewHistory::in_memory()` —
SQLite-shaped semantics without touching disk.

## Dependencies

`tokio` for spawn semantics, `sqlx` for the persistent history,
`async-trait` for object-safe `JobDispatcher` and `ReviewHistory`.
