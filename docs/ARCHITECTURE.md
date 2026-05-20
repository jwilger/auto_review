# auto_review Architecture

This document is the current architecture projection derived from the ADR event
stream. It describes the architecture new and updated code should follow. It is
not a rationale log; historical context and supersession details live in the ADRs.

## System shape

`auto_review` is a single-tenant Forgejo pull-request review bot implemented as a
Rust Cargo workspace of single-purpose crates. It is not a multi-tenant SaaS and
does not currently target GitLab, Bitbucket, a web dashboard, a web-based OAuth
installer, or fine-tuned models.

The primary runtime is a gateway service that receives Forgejo traffic, handles
chat commands, exposes operator endpoints, and dispatches semantic review work.
The public operator executable is `auto-review`; the service entrypoint is
`auto-review gateway`.

## Intake and dispatch

The gateway owns external request boundaries:

- Forgejo webhook requests pass through optional rate limiting, HMAC
  verification, bounded event classification, and bounded JSON failure handling.
- `POST /reviews/ci` is the authenticated CI intake path. It is enabled only when
  `AR_CI_REVIEW_TOKEN` is configured and receives the owner, repo, PR number,
  head SHA, and trigger metadata for the CI-validated revision.
- Chat commands are handled through webhook and polling paths because Forgejo
  does not reliably deliver every inline-thread reply event.

Normal semantic review is gated behind repository-selected CI prerequisites.
Regular Forgejo webhooks perform low-cost intake, bookkeeping, status updates,
and chat routing; they do not normally start expensive semantic review. Explicit
chat commands such as `@auto-review re-review` may force a review.

## Review pipeline

Semantic review follows a staged hybrid pipeline rather than a single monolithic
LLM agent:

```text
clone workspace
  -> deterministic triage
  -> context curation
  -> review generation
  -> self-heal JSON/schema validation
  -> pre-verifier severity-floor filtering
  -> verification
  -> post-verifier severity-floor filtering + path guard
  -> optional PR metadata quality check
  -> inline review and commit-status posting
```

The cheap-tier LLM triage module exists as an implementation seam, but the 1.0
runtime path relies on deterministic trivial-PR/file triage unless a later ADR and
code change wire LLM triage back into dispatch.

Deterministic linters, tests, and builds belong in CI before semantic review is
requested. The normal review runtime does not execute bundled linters, tests,
builds, generated code, or LLM-issued shell commands.

## Workspace and trust boundaries

Components receive only the workspace capability they need:

- Gateway webhook, chat, and CI-intake paths do not read the checkout.
- Workspace preparation uses hermetic Git subprocesses that ignore ambient host
  Git configuration, prompt helpers, templates, hooks, object/index overrides,
  and SSH command overrides.
- Context building and agentic verification use read-only, path-confined access
  under the clone root with symlink escape rejection and output/result caps.
- Agentic verifier tools are file read and search capabilities, not shell access.

Any future feature that executes repo-controlled code must introduce a
feature-specific sandbox design, fail-closed configuration, threat-model update,
and red-team tests before it is enabled.

## Persistence and repository context

The default single-tenant persistence model is embedded SQLite. SQLite-backed
stores persist review history, learnings, vector embeddings, and webhook delivery
deduplication state across restarts. In-memory stores remain appropriate for
tests and development paths where persistence is not the behavior under test.

Repository context uses the diff, changed paths, tree-sitter symbols, embeddings
when an embedding tier is configured, and persistent learnings. Co-change graph
support exists in `ar-index`, but the 1.0 review pipeline does not inject
co-change data into prompts. The review pipeline depends on the `VectorStore`
abstraction; `SqliteVectorStore` is the current persistent default. LanceDB or
another dedicated vector database should be revisited only when measured scale,
latency, or filtering requirements justify it.

## LLM routing

`ar-llm::Router` maps `ModelTier::{Reasoning, Cheap, Embedding}` to provider
implementations. The shipped provider surface is OpenAI-compatible and supports
hosted OpenAI-compatible APIs plus local or gateway-style backends such as
Ollama, vLLM, OpenRouter, Together, Groq, and similar endpoints.

## Observability

The gateway exposes separate operator routes with distinct semantics:

| Endpoint | Purpose |
| --- | --- |
| `GET /healthz` | Process liveness without downstream I/O. |
| `GET /readyz` | Readiness through configured runtime dependencies. |
| `GET /version` | Static deployment identity. |
| `GET /info` | Startup-time runtime configuration and posture snapshot. |
| `GET /metrics` | Prometheus text exposition. |

Review lifecycle metrics cross the crate boundary through `ReviewObserver` so the
orchestrator does not depend on the gateway's Prometheus implementation. Metric
names, dashboards, alert rules, and operator docs are a coupled contract.

## Development and CI tooling

`just` is the canonical command interface for routine development and CI checks.
Recipes such as formatting, clippy, tests, dependency policy checks, build checks,
and aggregate CI checks should call the underlying tools directly rather than
requiring `nix develop --command ...` inside the recipe.

Nix is an optional developer setup path and a supported CI provisioning mechanism.
Developers may use any environment that provides the required tools on `PATH`,
while `nix develop` provides the pinned tool environment used by the project. Nix
also remains responsible for production package assembly, embedded OCI/rootfs
runtime packaging, and NixOS module/service support.

Forgejo PR CI should run focused jobs built around the `just` recipes. CI may use
Nix to provision tools, but `nix flake check` is not the primary orchestration
interface for routine checks.

## Distribution and runtime isolation

The official out-of-the-box Linux distribution artifact is the signed
`auto-review` binary archive published through Forgejo releases. Release assets
must include checksums, signatures, signing material, SBOM/provenance metadata,
and verification instructions. Direct binary releases are temporarily Linux
`x86_64` only until a trusted Linux `aarch64` build and provenance path exists.

The project does not currently publish an official Docker/OCI image as a
first-class release artifact. Operators who want Docker or Podman images may
build their own image from the source or released binary.

For gateway execution from the official binary artifact, the architecture
requires embedded or linked OCI isolation by default. Bare-process gateway mode is
an explicit opt-out and must be reported clearly in startup logs, `/info`, and
diagnostic commands. If embedded OCI isolation cannot be established and the
operator has not opted out, startup fails closed.

Existing implementation in this area may be staged; new work should move toward
the binary-first, fail-closed embedded-OCI posture rather than expanding
bare-process gateway operation or reintroducing project-published image release
obligations.

## ADR event stream

Architecture decisions are recorded as ADR events under `docs/ADR-*.md`.
Accepted or rejected ADRs are immutable except for brief supersession metadata.
When the architecture changes, use the ADR workflow tools to create or update a
proposed ADR carrying the intended projection patch. The patch is applied to
this document only when that ADR is accepted.
