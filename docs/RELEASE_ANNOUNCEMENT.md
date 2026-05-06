# auto_review — release announcement copy

Drafts for three venues. Tweak host-specific details (URLs, version
numbers, "today") before posting.

---

## Codeberg / source repo README + tag-release notes

> **auto_review v0.1.0 — a self-hosted, Forgejo-native AI PR
> reviewer.**
>
> auto_review is the open-source counterpart to CodeRabbit /
> Greptile / Cursor BugBot — built explicitly for sovereignty-
> conscious teams running on Forgejo (or Gitea), and runnable
> entirely on your own infrastructure with local LLMs.
>
> **What it does:**
> - Listens for `pull_request` webhooks from your Forgejo
>   instance.
> - Runs after your selected CI linters/tests/builds have passed.
> - Sends the diff + repo-level `.auto_review.yaml`
>   guidance to a reasoning LLM.
> - Verifies the model's findings against the actual code,
>   dropping anything the diff doesn't corroborate.
> - Posts a single review with inline comments and an overall
>   summary.
>
> **What it deliberately doesn't do:** auto-merge, auto-approve,
> or auto-close. Every suggestion is advisory; humans stay in
> the loop.
>
> **LLM choice is yours.** Pluggable provider layer with
> implementations for Ollama (local), vLLM (local), OpenAI,
> Anthropic, and OpenRouter. A workstation with Ollama and a
> 32B-class coding model reviews real PRs end-to-end with no
> internet egress.
>
> **CI owns deterministic checks.** auto_review does not execute
> bundled linters in the gateway; projects keep objective pass/fail
> gates in Forgejo Actions and trigger semantic review afterwards.
>
> **What makes this different from PR-Agent:** verification
> agent (drops hallucinated findings before they hit your PR),
> two-tier triage (cheap-model classification skips trivial
> files, full pipeline for complex ones), persistent learnings
> store (`@auto_review remember/forget` chat commands),
> incremental reviews on push (only new commits get re-reviewed).
>
> Quickstart, runbook, and threat model in `/docs`. Issues and
> patches welcome at `<repo URL>`. AGPL-3.0-or-later.

---

## r/selfhosted post

**Title:** `auto_review: open-source AI PR reviewer for Forgejo
(self-hosted, local LLMs, AGPL)`

**Body:**

> Hi /r/selfhosted —
>
> Releasing **auto_review**, a CodeRabbit-equivalent PR
> reviewer that runs against your own Forgejo (or Gitea)
> instance, with no SaaS dependency. Open source, AGPL-3.0.
>
> **The problem:** every credible AI code-reviewer
> (CodeRabbit, Greptile, Cursor BugBot, Copilot review, Diamond)
> is GitHub-only and SaaS-only. Forgejo, the natural home for
> sovereignty-conscious self-hosters, has no equivalent. Qodo's
> PR-Agent has a Gitea provider but is single-LLM-call-per-tool —
> no CI-triggered semantic-review flow, no persistent
> learnings, no agentic verification.
>
> **What auto_review ships:**
> - Webhook intake → durable job dispatch → review pipeline →
>   posted review.
> - CI-triggered semantic reviews: run your linters/tests/builds in
>   Forgejo Actions, then call the gateway once deterministic gates pass.
> - **LLM router** with Ollama, vLLM, OpenAI, Anthropic, and
>   OpenRouter providers. Defaults assume you want local-only
>   (`qwen2.5-coder:7b` cheap-tier, `qwen2.5-coder:32b`
>   reasoning-tier).
> - **Verification agent** that double-checks each finding
>   against the actual code before posting; drops hallucinations
>   silently.
> - **Persistent learnings + symbol embeddings** (SQLite-backed;
>   ADR-0004 explains why SQLite is the default and how a
>   LanceDB drop-in fits behind the same trait).
>   `@auto_review remember "do X"` in any PR comment to add a
>   guideline.
> - Per-repo `.auto_review.yaml` for ignored paths and custom
>   review guidelines.
> - `/metrics` endpoint with Prometheus counters; Grafana
>   dashboard + Helm chart in `deploy/`.
>
> **What it doesn't do:** auto-merge, auto-approve, anything
> SaaS. Every suggestion is advisory. No telemetry; the only
> outbound calls are to the LLM provider you configure.
>
> **Deploy:** `docker compose up` next to your Forgejo
> instance. `auto-review auth init` mints the bot user, registers
> the webhook, and bootstraps `.auto_review.yaml`.
>
> Repo: `<repo URL>`
>
> Looking for: feedback, bug reports, and especially **real
> PR replays** — if you have a corpus of historical PRs from a
> repo you maintain, the docs/E2E_RUNBOOK.md describes how to
> point auto_review at them and surface false-positive vs
> false-negative rates against your own human reviews.

---

## lobste.rs submission

**Title:** `auto_review — open-source AI PR reviewer for Forgejo
with CI-triggered reviews and local LLMs`

**Body:**

> A Rust-implemented, self-hostable analogue of CodeRabbit
> targeting Forgejo. The interesting bits:
>
> - **CI-triggered semantic reviews.** Deterministic linters,
>   tests, and builds stay in Forgejo Actions, where projects
>   already control the required gates. auto_review runs after
>   those checks pass and focuses on semantic/fuzzy review.
>
> - **Hybrid pipeline + agentic** — not pure ReAct loops.
>   Triage (cheap-model classification) → context curation
>   (vector search + read-only workspace tools) → review
>   generation → verification (drops findings the diff doesn't
>   corroborate) → self-heal loop on JSON-schema validation.
>
> - **Local-LLM-only profile is a first-class deployment
>   target**, not an afterthought. Ollama / vLLM via OpenAI-
>   compatible endpoints; the cheap and reasoning tiers
>   default to qwen2.5-coder. Cloud overrides for OpenAI /
>   Anthropic / OpenRouter for teams who'd rather pay
>   per-token than run their own GPU.
>
> - **Persistence is SQLite, not LanceDB** — the trait
>   abstraction holds, and the rationale (no `protoc`
>   build dep, our scale doesn't need ANN) is recorded in
>   ADR-0004.
>
> - **Reproducible build via `flake.nix`.** Local dev
>   (`direnv allow` or `nix develop`) and CI
>   (`nix flake check`) run identical derivations; the rust
>   nightly snapshot is pinned by `flake.lock` so the whole
>   stack is hermetic.
>
> AGPL-3.0-or-later. `<repo URL>`.

---

## Cross-post checklist

Before hitting publish:

- [ ] Update `<repo URL>` placeholders to the actual Codeberg URL.
- [ ] Confirm the version tag (`v0.1.0` or whichever) is pushed
      and signed.
- [ ] Confirm `docker compose up` works against a clean checkout
      via the manual e2e runbook.
- [ ] If submitting to lobste.rs, expect tough comments on the
      AGPL choice and the hardening claims — be ready to point at
      `docs/THREAT-MODEL.md` and the Git/workspace red-team tests.
- [ ] Don't pre-emptively address future Greptile/CodeRabbit
      feature parity — keep the focus on what's shipped today
      and the threat-model differences (sandboxing, sovereignty,
      no SaaS).
