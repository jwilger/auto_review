# ar-chat

`@<bot>` chat command surface. Parses mentions in PR comments and
inline-thread replies, dispatches to the matching action, and
replies in-thread. Used by both the gateway's webhook handler
(top-level PR comments) and the `ChatPoller` (inline-thread
mentions, which Forgejo's `pull_request_review_comment` webhook
doesn't fire reliably for — gitea#26023).

## Public surface

| Module | Content |
|--------|---------|
| `command::ChatCommand` (enum) | Variants: `NotMentioned`, `Help`, `Remember(text)`, `Forget(id)`, `ReReview`, `Autofix`, `Docstring`, `Tests`, `Freeform(text)`. |
| `command::parse_chat_command` | Robust mention parser. Bot name is configurable — the gateway plumbs `bot_name` from `AR_BOT_NAME` env. |
| `handler::ChatHandler` | Owns the dependencies (`ForgejoClient`, `LlmRouter`, `LearningsStore`, optional `JobDispatcher` for `re-review`) and dispatches `handle(ctx, command)`. |
| `handler::ChatContext` | `(owner, repo, issue_number)` — passed to every action. |
| `handler::ChatError` | Forgejo / LLM / chat-flow errors. |

## Commands

| Command | What it does |
|---------|-------------|
| `help` / `?` / `-h` | Lists every command with a one-line description. |
| `remember <text>` | Saves a learning to the `LearningsStore`. Future reviews retrieve it via RAG. |
| `forget <id>` | Deletes a learning by id. |
| `re-review` | Bypasses the orchestrator's `last_reviewed_sha` dedup and queues a fresh review of the current head SHA. Requires the optional `dispatcher` dep. |
| `autofix` | Generates suggested patches for the bot's most recent findings. Posted as a comment, not pushed. |
| `docstring` | Suggests rustdoc / pydoc / etc. for newly-added public APIs. |
| `tests` | Suggests scaffolded test cases for added code. |
| Freeform | Anything else gets routed to the cheap LLM tier as PR-context Q&A. |

## Tests

`cargo test -p ar-chat` covers the parser (~20 tests on edge
cases: mention without command, multi-word arguments, case
insensitivity) and the handler (~15 tests on each command's
Forgejo-side calls + error paths). All LLM and Forgejo calls go
through `wiremock` and `CannedProvider`.

## Dependencies

`async-trait` for `LlmProvider` callbacks, `serde_json` for
prompt rendering of suggestion outputs.
