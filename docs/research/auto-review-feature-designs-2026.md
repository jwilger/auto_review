# auto_review feature designs, 2026

This report consolidates six design studies (trust and cleanup, measurement, output and noise, rules and gating, agentic architecture, learning and local review) into one backlog for the agent delivery factory. It records the competitor evidence behind each design, the design chosen for auto_review, the maintainer's decisions and every default chosen during consolidation, and the open questions. Code references were checked against `main` at 158ecbf on 2026-11-16. The backlog keys (for example `SEC-03`) map to GitHub issues, and the table at the end lists every item.

## How the backlog is ordered

The factory builds the oldest ready, unblocked issue first, so creation order is priority order. The order is:

1. **Live security defects.** The bot token is stored in the clone's `.git/config` and readable by workspace tools. A PR's own `.auto_review.yaml` can disable or steer its review. Untrusted PR text sits in the review prompt without fences. Any commenter can get an approval when the bot is not requesting changes. Chat `forget <id>` can delete another repository's learnings. The learnings and vector stores are not repository-scoped.
2. **Approval default change.** The maintainer decided that approval becomes `shadow` by default now, so the merge recommendation, stale-review dismissal and approval policy follow the security block.
3. **Measurement foundations and bench enablement.** Finding identity, a findings store, outcome evaluation, feedback verbs, cost and volume metrics, a host-free review core, a bench that can run workspaces, and the Martian offline set.
4. **Cleanup.** Small drift fixes and removals.
5. **The rest, in the order of the landscape shortlist:** noise controls, finding format, dedupe and persistent summary, guideline ingestion, the remaining approval work and issue #1, linked issues, agentic generation with cost guardrails, feedback suppression, custom pre-merge checks and issue #2, injection flags, effort tiers, local review, hypothesis fan-out, and the learned-rule lifecycle.
6. **Items that first needed a human** (live-instance verification, GitHub App changes, bench-gated default flips, deferred features) were created with `ready: false` in bucket 3, last. On 2026-11-16 the maintainer resolved each one so an agent can run it (see Decisions 23–29); only the Forgejo v18 verification still waits, for the v18 release.
7. **One tracking epic per theme**, last, because each epic lists its slices.

Slice bodies refer only to earlier items, so issue numbers can be substituted in creation order.

## Theme SEC: trust and security

### What leaders do

- Cursor Bugbot reads `.cursor/config/bugbot.yaml` from the base branch, "so a PR cannot change how Bugbot reviews itself" ([Bugbot docs](https://cursor.com/docs/bugbot)). Anthropic Code Review reads `REVIEW.md` "from the pull request's base branch or from the default branch, never from the pull request's own changes" ([Claude Code docs](https://code.claude.com/docs/en/code-review-customize)). CodeRabbit reads config from the PR branch ([CodeRabbit YAML](https://docs.coderabbit.ai/getting-started/yaml-configuration)); its 2024 RCE came from PR-supplied linter config ([Kudelski](https://kudelskisecurity.com/research/how-we-exploited-coderabbit-from-a-simple-pr-to-rce-and-write-access-on-1m-repositories)).
- Bugbot "doesn't read the contents of files whose path looks like a credential file" (`.env`, `id_rsa`, `*.pem`, `credentials.json`, `*.tfvars`, Terraform state) and reviews templates such as `.env.example` normally ([Bugbot docs](https://cursor.com/docs/bugbot)).
- CodeRabbit scopes learnings `local`, `global` or `auto` and redacts credentials before storing them ([CodeRabbit learnings](https://docs.coderabbit.ai/knowledge-base/learnings)). Bugbot learned rules are per repository.
- The 2026 Ghostcommit attack hid instructions in an AGENTS.md file and PNGs that passed CodeRabbit and Bugbot ([CSA](https://labs.cloudsecurityalliance.org/research/csa-research-note-ghostcommit-image-prompt-injection-ai-code/)).

### Chosen design

- **Clone credential.** `git remote remove origin` runs right after checkout (`crates/ar-review/src/workspace.rs:118-119`), so the token never stays in `.git/config`. As a second layer, a shared predicate `ar_index::sensitive::is_credential_path` denies `.git/**`, `.env*` (except template names), key and certificate files, tool credential files and IaC state to `read_file` and `search`. It checks both the requested path and the canonical path. Credential-file hunks are withheld from review and chat prompts and listed in a deterministic note.
- **Base-branch config.** A new `crates/ar-review/src/trusted_files.rs` is the only runtime loader for repository-controlled instruction files. It reads `.auto_review.yaml` (then `.yml`) through `ReviewHost::get_file_content` at the PR's base branch name, caps the body at 64 KiB, percent-encodes the `ref`, and falls back to defaults with a warning on errors. `run_review_job` fetches the PR once and carries the base ref and SHA to later stages, so `ReviewJob` and the AgentCore payload stay unchanged. A PR that edits the config gets a note that the base config was used, with strict-validation problems in the head version.
- **Prompt fences.** `ar_prompts::fence_untrusted` wraps every untrusted section (PR title and body, prior discussion, repo context and learnings, guidelines, and later linked issues, check instructions and tool results) in open and close markers that carry a per-review nonce. It neutralizes forged markers and strips bidi, zero-width and Unicode tag characters. The system prompt says fenced text is data.
- **Chat approvals.** Until the approval policy exists, only base-branch `override_approvers` can obtain an approval through chat.
- **Repository scope.** Learnings gain `repo` (lowercased `owner/repo`) and `origin_pr`. Retrieval, `remember`, corrections and chat `forget` are scoped to the current repository. Legacy rows are backfilled from the "Repository X only." prefix, or else stay unscoped and excluded until an operator assigns them. The vector store is keyed by repository, and the old cache table is dropped and rebuilt.

### Security and threat-model impact

New or changed threats: **T3a** PR-supplied config disables or narrows its own review; **T4a** credential reads inside the workspace (`.git/config`, `.env`, keys); T5 mitigation (remote removed after checkout); T6 (repository scoping, scoped `forget`, and a correction of the asset table's false claim that chat commands are gated to collaborators). The existing IDs T1a, T5a, T5b and T6a are already taken, which is why the new entries use T3a, T3b and T4a.

## Approval policy (theme RULE, built early)

### What leaders do

PR-Agent computes its merge recommendation deterministically and lets the model only make it more cautious ([PR-Agent /review](https://docs.pr-agent.ai/commands/review/)). Macroscope approves only when eligibility and a zero-issue review both pass. cubic offers a shadow mode. Greptile approves low-risk PRs after a clean review ([Greptile changelog](https://www.greptile.com/docs/changelog)). CodeRabbit's automatic approvals are blocked by error-mode checks ([CodeRabbit pre-merge checks](https://docs.coderabbit.ai/pr-reviews/pre-merge-checks)). Bugbot and Claude Code Review never approve. auto_review today approves every review without an Error finding (`crates/ar-review/src/mapping.rs:18-27`).

### Chosen design

- `crates/ar-review/src/approval.rs` computes `MergeRecommendation::{Ready, NeedsHumanReview(reasons), ChangesRequested(reasons)}` from severity counts, error-mode check results, changed-line and file limits, built-in and configured critical paths (agent-config files, `.auto_review.yaml`, `.github/**`, `.forgejo/**`, `**/CODEOWNERS` and similar), the author (never the bot itself), and injection or asset flags.
- `approval.mode: on | shadow | off`, **default `shadow`** (maintainer decision). `on` with `Ready` posts Approved. Every other non-blocking case posts Comment, and shadow mode says "Would have approved" or "Would not approve: reasons". Error findings and error-mode check failures post Request changes in every mode.
- **Stale change requests.** When the bot's latest review requested changes and the new review is not blocking, the bot dismisses its own stale review (`POST …/pulls/{index}/reviews/{id}/dismissals` on Forgejo, `PUT …/pulls/{n}/reviews/{id}/dismissals` on GitHub). If dismissal is refused, it falls back to an Approved review whose body says it only clears the earlier change request and is not a merge-readiness approval. It never dismisses other users' reviews.
- Chat approvals follow the same policy: override approvers always, and other commenters only in `on` mode when the recommendation is `Ready`.

## Theme MEAS: measurement and benchmark

### What leaders do

- Cursor judges resolution with an LLM at merge; resolution rate rose from 52% to over 70% while bugs flagged per run rose from 0.4 to 0.7 ([Cursor](https://cursor.com/blog/building-bugbot)).
- Greptile compares first and last commits; its address rate rose from about 19% to 55%+ after an embedding filter ([Greptile](https://www.greptile.com/blog/make-llms-shut-up)), and from 52% to 66% in its v5 A/B test ([Greptile v5](https://www.greptile.com/blog/greptile-v5)).
- Graphite tracks action rate and warns that an unacted-on comment is not automatically a failure, and that under 4% of comments get a downvote ([AI Engineer talk](https://ai.engineer/talks/ai-powered-entomology-lessons-from-millions-of-ai-code-reviews)).
- Copilot added Addressed / Won't fix / Incorrect resolution reasons ([GitHub changelog 2026-08-27](https://github.blog/changelog/2026-08-27-copilot-code-review-resolution-reasons-and-expanded-capabilities/)).
- Martian's offline set has 50 PRs and 173 golden comments with severity and category but no file or line; it scores with an LLM judge, category profiles and F-beta ([Martian benchmark](https://github.com/withmartian/code-review-benchmark)).

### Chosen design

- **Identity.** Each posted finding gets two 12-hex fingerprints, `a` (path, category and normalized anchor-line text) and `t` (path and normalized title), plus a 32-hex `finding_id` per review run. The inline comment carries `<!-- auto-review:finding v1 id=… a=… t=… -->` and a visible footer `ar:<8 hex> · reply @auto-review addressed|wontfix|incorrect`. `<!--` in model text is neutralized so the model cannot forge markers.
- **State.** A `FindingStore` in `ar-index` is the source of truth for finding state. SQLite lives in its own file (`AR_FINDINGS_DB`, `:memory:` to opt out) with tables `review_runs`, `review_findings` and `finding_feedback`. DynamoDB uses one table partitioned by repository (`pk = REPO#{provider}#{owner}/{repo}`, `sk = PR#{pr}#RUN#…|FINDING#…|FEEDBACK#…`), so both per-PR and per-repository queries are a Query. Hidden markers carry identity only, used to bind comment ids and to rebuild state when the store is missing. There is no ledger in the summary comment.
- **Outcomes.** One shared module, `crates/ar-review/src/addressed.rs`, holds `region_touched` (a deterministic overlap test against a compare diff, ±3 lines of context) and a batched Cheap-tier judge with verdicts `addressed | not_addressed | unclear`. Untouched findings are `not_addressed` without an LLM call. The close-time evaluator runs on Forgejo `pull_request` `closed`, GitHub `pull_request` `closed`, an AgentCore invocation, or a CLI backfill. Human verbs always win. Reactions and thread resolution are stored in separate columns. Addressed rate counts merged PRs only, excludes unknown and pending, and is reported next to findings per review.
- **Feedback verbs.** `@auto-review addressed|wontfix|incorrect [ar:<id>] [reason]`. The target is resolved from the explicit id, then the GitHub `in_reply_to_id`, then the Forgejo quote anchor. Lookups are scoped to the PR.
- **Metrics.** Bounded labelled counter families (severity, category, outcome, verb, mode), histograms for cost and findings per review, recording rules and dashboard rows, and a contract test that pins the deploy files to the exposed metrics. `auto-review stats` reads the store for durable per-repository reports.
- **Bench.** A host-free review core (`run_review_core`) is shared by the gateway, the bench and local review. Fixtures can carry workspace snapshots, `also_valid` findings and replayed findings, and results include cost. Martian data is downloaded on demand at a pinned commit and checksums, not vendored, because the diffs carry FSL, AGPL and GPL licences. It is scored with Martian's judge prompt and Strict/Core/All profiles, with an adjudication overlay for valid findings missing from the gold set.

### Security impact

The judge prompts frame diff and finding text as untrusted (T3). Verbs and reactions are new durable writes open to commenters, so they are scoped per PR, attributed and capped (T6). Finding excerpts have retention and purge (T6). The judge-call cap bounds cost (T8).

## Theme OUT: review output and noise

### What leaders do

- Comments with inline code suggestions were useful 75.5% of the time against 64.6% without, and longer comments were slightly less useful ([arXiv 2607.21997](https://arxiv.org/html/2607.21997v1)). Concise comments with code snippets were more often acted on ([arXiv 2508.18771](https://arxiv.org/abs/2508.18771)).
- Graphite suppresses technically correct but unwanted categories. CodeRabbit has `quiet`, `chill` and `assertive` profiles ([config reference](https://docs.coderabbit.ai/reference/configuration)). PR-Agent defaults to 6 findings. Gemini exposes `max_review_comments`.
- PR-Agent embeds hashed markers to skip re-posting and keeps a persistent comment with addressed and still-open findings ([PR-Agent](https://github.com/The-PR-Agent/pr-agent/blob/main/pr_agent/settings/configuration.toml)). Copilot, CodeRabbit and Claude Code Review resolve threads a later commit addressed ([GitHub changelog 2026-09-11](https://github.blog/changelog/2026-09-11-auto-resolution-and-analysis-updates-in-copilot-code-review/)).
- Forgejo has had multi-line comments since v16.0.0 (PR #12582). Suggestion blocks (#13023), thread replies (#14850) and resolve (#14873) are merged for v18.0.0, which is not released yet ([Forgejo PRs](https://codeberg.org/forgejo/forgejo/pulls/14873)).

### Chosen design

- **Categories.** `ReviewFinding` gains `title` and a closed `category` enum: correctness, security, reliability, performance, compatibility, ci_coverage, tests, documentation, refactor, naming, style. A missing category deserializes as `correctness`. This one enum also serves metrics, suppression, fan-out and the bench mapping.
- **Profiles.**

  | profile | floor | max inline | suppressed |
  |---|---|---|---|---|
  | quiet | warning | 3 | all but correctness, security, reliability, compatibility |
  | standard (default) | `AR_SEVERITY_FLOOR` or warning | 6 | tests, documentation, refactor, naming, style |
  | thorough | note | 20 | none |

  Explicit `review.*` keys override the profile. `security` and `correctness` can never be suppressed. Overflow findings are listed in a collapsed section, and suppressed findings are only counted. The review event considers overflow findings too.
- **Format.** Real line ranges (GitHub `start_line`, Forgejo `extra_lines_count` on v16+). Anchors are pre-validated against right-side hunks, so GitHub's 422 fallback no longer folds every comment into the body. At most one `suggestion` block per comment, with the same markup on both hosts (it renders as a plain block on Forgejo before v18). Validated `file:line` evidence goes in a collapsed "Why this was flagged". A deterministic "Prompt for AI agents" goes on each finding, with a combined prompt in the summary.
- **Persistent summary.** One bot issue comment per PR (Forgejo cannot edit review bodies), edited in place. The review body shrinks to the verdict and a link. A new review is posted only when there are new inline comments or the verdict changes.
- **Dedupe and addressed.** A new finding is skipped when its `a` or `t` matches an open bot-authored finding (marker identity plus store state). Addressed findings are edited to "✅ Addressed in <sha>" on every host. GitHub, and Forgejo v18+, also reply in the thread and resolve it; Forgejo v11–v17 get the edit only.
- **Summary content.** A deterministic verdict line with the merge recommendation and counts, a collapsed per-file table, a review-effort estimate from the same generation call, and "Guidelines applied".

### Host APIs added to `ReviewHost`

`capabilities()` and `bot_login()` (Forgejo `GET /api/v1/version` and `GET /user`; GitHub `GET /app`), `list_inline_review_comments`, `edit_issue_comment` and `edit_review_comment` (Forgejo `PATCH /issues/comments/{id}` for both; GitHub `PATCH /issues/comments/{id}` and `PATCH /pulls/comments/{id}`), `reply_to_review_comment` and `resolve_review_thread` (GitHub REST replies plus GraphQL `resolveReviewThread`; Forgejo v18 `POST /pulls/{index}/comments/{id}/replies` and `POST /pulls/comments/{id}/resolve`), `dismiss_review`, `get_issue`, and `list_comment_reactions`. Each has a default implementation that returns an error, so fakes keep compiling.

### Security impact

Suggestions and agent prompts turn model text into text a human or a downstream agent may act on (T9). They get deterministic wrappers, fences sized above any backtick run, length caps and human-only application. Edits, replies and resolves touch only bot-authored comments, chosen by `bot_login` plus marker and never by model output.

## Theme RULE: rules, requirements and gating

### What leaders do

Copilot reads `.github/copilot-instructions.md` and `applyTo`-scoped `.instructions.md` files from the head branch ([GitHub Docs](https://docs.github.com/en/copilot/how-tos/configure-custom-instructions/add-repository-instructions)). Codex concatenates AGENTS.md from root to leaf under 32 KiB and reads a `## Code Review Rules` section ([OpenAI](https://learn.chatgpt.com/docs/agent-configuration/agents-md)). Anthropic and Bugbot read from the base branch, scope by ancestor directory and report what was skipped. CodeRabbit offers `path_instructions`, natural-language pre-merge checks with off/warning/error modes and an Inconclusive result, and linked-issue assessment ([pre-merge checks](https://docs.coderabbit.ai/pr-reviews/pre-merge-checks)). Issue and PR text gives larger gains than code context alone ([arXiv 2511.07017](https://arxiv.org/abs/2511.07017)).

### Chosen design

- **Guideline files.** Read only from the base tree. `AGENTS.md` and `CLAUDE.md` (root and ancestors of changed files), `REVIEW.md` and `.cursor/BUGBOT.md`, Copilot repo and `applyTo` files, and AGENTS.md Code Review Rules sections. `path_instructions` comes from `.auto_review.yaml`. Every source is fenced, has an 8 KiB per-source cap inside a 32 KiB total budget in a fixed order, and is reported with its status in "Guidelines applied". Review-only sources also reach the verifiers as a digest.
- **Linked issues.** Only closing keywords in the PR title or body, same repository by default (cross-repository references could leak private issues through an installation token), at most 3 issues of 4 KiB each, fenced. Issue assessment is a built-in check, `warning` by default.
- **Pre-merge checks.** `pre_merge_checks.custom[]` with `{name, mode, instructions}`, at most 10 checks of 4 KiB. One Cheap-tier call per check against a closed schema. A failure, error or schema problem gives `inconclusive`, never `pass`. An error-mode `fail` posts Request changes. Each check posts a commit status `auto_review/check/<slug>`; GitHub has no warning state, so warning results post `success` with a "warning:" description. The metadata gate becomes the first built-in check with a mode. Override approvers can waive a check for the current head SHA. On GitHub, each check is also published as a check run (`auto_review / <slug>`, warning → `neutral`, inconclusive → `action_required`), on by default (Decision 23); installations that have not accepted `Checks: Read & write` fall back to the commit status.
- **Issue #2, structured review agents.** Kept as its own feature. `review_agents` render as fenced, labelled checklists in the main generation prompt, and findings can name the agent; names that are not configured are dropped.
- **Issue #1, re-check on edit.** A title or body edit re-runs only the Cheap-tier metadata evaluation for an already-reviewed head and posts only when the verdict flips, through the same approval policy. On GitHub this needs the App to subscribe to Pull request events.
- **Injection flags.** Deterministic flags for added images and binaries, instruction-like text in agent-config and docs files, and a reasoning-model advisory when agent-config files change. The flags are added after the verifier and feed the merge recommendation.

### Security impact

New **T3b** covers injection through guideline and agent-config files and binary assets (base-only reads, fences, caps, flags; image content is not inspected). T9 is rewritten so that error-mode checks may promote the review event, and inconclusive never passes. Linked issues join the T3 inputs, and the cross-repository disclosure risk is added to T9. Check count and size caps bound cost (T8).

## Theme AGENT: agentic architecture, effort and cost

### What leaders do

Bugbot's switch to a fully agentic, tool-calling reviewer gave its largest gains. Its agent was "too cautious" until prompts pushed it to investigate, and tool design had an outsized effect ([Cursor](https://cursor.com/blog/building-bugbot)). OpenAI found repository access reduced false alarms ([OpenAI](https://alignment.openai.com/scaling-code-verification/)). Copilot (Lite/Balanced/Max), Greptile, Claude and Bugbot all offer effort tiers. Greptile v5 and Sentry investigate a few hypotheses in parallel (Sentry caps them at three) ([Sentry](https://blog.sentry.io/building-a-code-review-system-that-uses-prod-data-to-predict-bugs)). Copilot's Lite ensemble raised addressed high-severity comments by 47% at 8% lower cost.

### Chosen design

- **One loop.** A bounded JSON-action loop (`agent_loop.rs`) is shared by the agentic verifier, agentic generation and fan-out investigators. Native tool calling is not used, because vLLM needs server flags for it and its combination with `response_format` is undocumented. Tool results are fenced as untrusted, and a cumulative byte budget forces the terminal step.
- **New read-only tools.** `list_dir`, `find_symbol` (from the tree-sitter index), `find_references` (lexical) and `find_callers` (tree-sitter call sites). The credential denylist applies to all of them.
- **Agentic generation.** Behind `AR_AGENTIC_GENERATION_TURNS` (default 0, which is today's behavior). After exploration, the final call returns the unchanged `review.json`. The verifier never sees the exploration transcript.
- **Cost budget (maintainer decision 3).** A `CostBudget` on the `Router` holds a USD cap and/or token cap. At the soft limit (60% by default), exploration ends, fan-out stops launching investigators and the agentic verifier must give a verdict. At the hard cap the router refuses further calls. The review then posts the findings verified so far with a "budget exhausted" note, counts the unverified ones, never approves, and sets a `pending` commit status (not `error`).
- **Effort.** `review.effort: quick | standard | deep` (quick: Cheap generation, no tools, $0.25; standard: today's pipeline, no cap; deep: 12 turns, agentic verifier, $5). `review.cost_cap_usd` overrides the tier cap. Chat `re-review deep` is allowed only for override approvers.
- **Fan-out.** Runs only at `deep` effort: shadow first, then post. A planner proposes at most 3 hypotheses (5 maximum). Investigators run per hypothesis with file-scoped hunks and bounded concurrency. A deterministic merge clusters and ranks the results, then the normal verifier runs. A process-wide in-flight limit, `AR_LLM_MAX_INFLIGHT`, protects self-hosted backends.
- **Defaults change only on bench rules,** run by a factory agent with the models in the agent LLM env file (Decision 25) on at least 12 fixtures (at least 5 cross-file), 3 runs each. G1, agentic verifier default: drops at least 2 more false positives, no more true positives, ≤ $0.10 added per review, ≤ 30 s p99. G2, agentic generation default: recall +0.05 with at least 2 extra cross-file true positives, precision −0.05 at most, mean cost ≤ $0.50 per review (Decision 26), ≤ 60 s p99. G3, fan-out posting: recall +0.05, post-verifier precision −0.03 at most, p95 cost within the deep cap.
- The co-change graph cannot work on a `--depth=1` clone and is deleted, together with the unwired LLM triage.

### Security impact

Tool results are a new injection channel (T3): they are fenced, the schema and severity-derived event are unchanged, the path guard and caps apply. New tools get red-team cases (T4). Fan-out multiplies calls (T7 and T8), bounded by N, concurrency, the in-flight limit and the cost cap. The current T8 text claims a "per-PR token budget" that does not exist; the cost-cap slice corrects it.

## Theme LEARN: learning loop and local review

### What leaders do

Greptile blocks a comment that resembles at least 3 downvoted comments; its address rate went from 19% to 55%+, with no published threshold ([Greptile](https://www.greptile.com/blog/make-llms-shut-up)). Sentry filters suggestions similar to downvoted ones per repository. Bugbot learned rules turn on and off automatically ([Cursor changelog](https://cursor.com/changelog/04-08-26)). CodeRabbit has a pending/approved/disapproved queue with `approval_delay` and no expiry. CodeRabbit CLI, cubic and Bugbot review local changes, and CodeRabbit's agent mode exits 0 passed, 1 incomplete, 2 blocked ([CodeRabbit agent mode](https://docs.coderabbit.ai/cli/agent-mode)).

### Chosen design

- **Feedback suppression.** Embeddings of posted findings (`"<category or severity>: <message>"`, no path) are stored as columns of the findings store. A candidate is suppressed when at least `K = 3` explicitly dismissed findings (Won't fix or Incorrect) from at least 2 PRs match it at cosine ≥ τ = 0.85 within 180 days, with the same embedding model, and fewer than K addressed findings match. Error, security and correctness findings are never suppressed. The mode is `shadow` by default, with logged similarities for calibration. Each finding keeps its shadow decision, and the outcomes of would-suppress findings are exported as a metric; `enforce` becomes the default only by the fixed rule in Decision 27. Enforce mode discloses every held-back finding in the summary.
- **Learned rules.** Learnings get a status (candidate, active, disabled, rejected), path glob, provenance and statistics. Inferred rules are drafted from dismissal clusters as candidates that need chat approval from `learnings.approvers` (falling back to `override_approvers`). `forget` and the lifecycle commands are gated to the same list. Findings cite learnings they used through `learning_refs`. Rules are auto-disabled after 10 attributions with under 30% acceptance, or 3 new cluster dismissals after activation.
- **Local review.** `auto-review review local` diffs against the merge-base, including uncommitted tracked changes, using hardened git (`core.fsmonitor=false`, `--no-ext-diff`, `--no-textconv`). It reads `.auto_review.yaml` from the working tree, because the local user is the trust principal. JSON output has `schema_version: 1`, and the gate exits 0, 1 or 2. Agentic verification and RAG read only git-tracked files. An MCP stdio server exposing `review_diff` is built on top of local review (Decision 28).

### Security impact

New **T6b**, suppression poisoning: never-suppress classes, K across at least 2 PRs, repository scope, disclosure and an operator off switch. T6 is updated for gated lifecycle commands and provenance. New **T10**, local review of a developer working tree: tracked files only, repository-local git config neutralized, no code execution.

## Theme CLEAN: cleanup, drift and live verification

The landscape report lists four drifts. The severity-floor doc comment is fixed together with host-neutral chat wording. The USER-GUIDE "cannot approve" claim is rewritten by the approval-policy slice, and the "secrets" claim by the credential slices. The co-change and triage claims go with the code deletion. The review system prompt's "Forgejo PR" is fixed by the fencing slice. The docstring command becomes a fixed retirement notice: CodeRabbit retired docstring generation on 2026-11-11 ([CodeRabbit changelog](https://docs.coderabbit.ai/changelog)). `AGENTS.md` and `.claude/rules/*.md` are factory-class paths, so feature slices do not edit them. Two factory-class sync items collect those edits. Existing issue #13 also edits `AGENTS.md` and adds an ADR, so ADR numbers are allocated at implementation time.

## Cross-cutting design

### Repository configuration (read from the base branch)

```yaml
enabled: true
guidelines: "…"
ignored_paths: []
override_approvers: []
pr_metadata_check: {enabled: true, mode: error, checks: {body_required: true}, additional_rules: []}
review:
  profile: standard            # quiet | standard | thorough
  severity_floor: warning      # overrides AR_SEVERITY_FLOOR
  max_inline_comments: 6
  suppress_categories: []
  allow_categories: []
  agent_prompts: true
  effort: standard             # quick | standard | deep
  cost_cap_usd: 5.0            # overrides the operator per-tier default
approval:
  mode: shadow                 # on | shadow | off
  max_finding_severity: warning
  max_changed_lines: 0
  max_changed_files: 0
  critical_paths: []
  allowed_authors: []
  exclude_authors: []
guideline_files: {enabled: true, sources: [agents_md, claude_md, review_md, copilot, bugbot], agents_md: full}
path_instructions: [{path: "db/migrations/**", instructions: "…"}]
linked_issues: {enabled: true, max_issues: 3, allow_cross_repo: false}
pre_merge_checks:
  issue_assessment: warning
  custom: [{name: migration-safety, mode: error, instructions: "…"}]
review_agents: {security: {items: ["…"]}}
injection_flags: {enabled: true}
feedback_suppression: {enabled: true}
learnings: {approvers: [], require_approval: false, infer: true}
```

### Precedence (maintainer decision 2)

Repository config, read only from the base branch, wins over operator defaults: `AR_SEVERITY_FLOOR`, `AR_EFFORT_DEFAULT` and `AR_REVIEW_COST_CAP_USD_{QUICK,STANDARD,DEEP}`. Operator variables are defaults only; no operator variable caps a repository key. The base-branch slice lands before any slice that adds a repository-controllable severity, noise, effort or cost key.

### Pipeline order after all themes land

Generation (optionally agentic), then category filter, pre-verifier severity floor, feedback suppression (shadow or enforce), verifier (simple or agentic), post-verifier severity floor, changed-path guard, deterministic injection-flag findings, dedupe against open bot findings, rank and cap, pre-merge checks, merge recommendation and approval policy, mapping, post, then summary comment, statuses, findings store and acknowledgement of addressed findings. Close-time outcome evaluation runs separately.

### New operator environment variables

`AR_FINDINGS_DB`, `AR_FINDING_FEEDBACK_HINT`, `AR_OUTCOME_EVALUATION`, `AR_OUTCOME_MAX_JUDGED`, `AR_REVIEW_COST_CAP_USD`, `AR_REVIEW_TOKEN_CAP`, `AR_REVIEW_COST_SOFT_FRACTION`, `AR_REVIEW_COST_CAP_USD_{QUICK,STANDARD,DEEP}`, `AR_EFFORT_DEFAULT`, `AR_AGENTIC_GENERATION_TURNS`, `AR_AGENTIC_VERIFIER_CONCURRENCY`, `AR_LLM_MAX_INFLIGHT`, `AR_HYPOTHESIS_FANOUT`, `AR_HYPOTHESIS_MAX`, `AR_FANOUT_CONCURRENCY`, `AR_MAX_FINDINGS_DEEP`, `AR_FEEDBACK_SUPPRESSION`, `AR_FEEDBACK_SIMILARITY`, `AR_FEEDBACK_MIN_MATCHES`, `AR_FEEDBACK_MAX_AGE_DAYS`, `AR_LEARNING_MIN_ATTRIBUTIONS`, `AR_LEARNING_MIN_ACCEPTANCE`, `AR_LEARNING_MAX_CLUSTER_DISMISSALS`, `AR_GITHUB_CHECK_RUNS` (default `true`). All are parsed with `read_non_empty_env` or `parse_env` in `crates/ar-gateway/src/startup.rs`, and documented in `docs/OPERATIONS.md` and `deploy/systemd/auto_review.env.example`.

### New ADRs

ADRs are append-only (ADR-0016), so each of these slices adds a new ADR and updates `docs/ARCHITECTURE.md`: SEC-03 (base-branch trusted loader), SEC-06 (repository-scoped learnings), OUT-01 (capability probing and Forgejo degradation), RULE-03 (approval policy), MEAS-04 (finding identity and state model), MEAS-09 (webhook-triggered follow-up work that is not review dispatch), CLEAN-03 (dropping co-change and LLM triage from ADR-0001's pipeline), AGENT-04 (per-review cost budget), AGENT-06 (tool-using generation), LEARN-02 (feedback suppression), RULE-17 (repository-configured gating checks), LEARN-04 (local review trust principal) and LEARN-10 (inferred rules need human approval).

## Decisions

Maintainer decisions:

1. **Approval default is `shadow` now.** The bot stops approving by default and reports "would have approved" until a repository sets `approval.mode: on`. It clears its own stale Request changes by dismissal. When dismissal is refused, it falls back to an Approved review whose body states it only clears the earlier change request; this fallback is the one remaining path to an automatic approval in shadow mode, and a bucket-3 item verifies which path each host takes. Chat approvals are limited to override approvers (SEC-05) until the policy can say `Ready` (RULE-10). Deployments whose branch protection relies on the bot's approval must set `approval.mode: on`; the RULE-03 commit body says so, so the generated release notes do too.
2. **Repository config wins over operator defaults.** See Precedence. The maintainer confirmed there are no operator ceilings: `AR_APPROVAL_MAX_MODE` and `AR_EFFORT_MAX` from the notes are dropped. Only people who can change the base branch can raise effort, cost caps or approval mode.
3. **Cost cap.** Soft limit at 60% (`AR_REVIEW_COST_SOFT_FRACTION=0.6`) ends exploration and fan-out and forces verifier verdicts. At the hard cap the bot posts the findings verified so far with a "budget exhausted" note, counts the unverified findings without posting them, never approves, and sets a `pending` commit status on both hosts. GitHub commit statuses have no neutral state, so `pending` is used everywhere. If no valid review exists yet, it posts a comment-only review with the note. Counter: `auto_review_reviews_budget_exhausted_total`.
4. **Forgejo capabilities.** Probe `GET /api/v1/version` once per client. Major version ≥16 enables multi-line comments; ≥18 enables suggestion blocks, thread replies and resolve. An unparseable version means no optional capabilities. On v11–v17 the same suggestion markup renders as a plain code block, and addressed findings are marked by editing the original comment.

Defaults chosen during consolidation, where the notes disagreed or left a decision open (the notes' recommended default is used unless stated):

5. **Base content is the base branch tip by name.** Config and single files are read through the host API at `base.ref_name`. The guideline tree is read by fetching `refs/heads/<base>` (with a strictly validated name) into the workspace before the clone remote is removed, and the resolved SHA is recorded as provenance. A non-404 fetch error falls back to defaults with a warning. Protected-branch checks are not done.
6. **Finding state.** The findings store is the source of truth, and the hidden marker carries identity (the measurement notes' store plus the output notes' markers). The output notes' summary ledger is dropped. Dedupe joins bot-authored markers with store state; without a store, every unaddressed bot finding counts as open. `FindingStore` lives in `ar-index` so that `ar-review`, `ar-orchestrator` and `ar-chat` can all use it. It uses a separate `AR_FINDINGS_DB` file.
7. **Fingerprints.** Two fingerprints (`a` anchor text with category, `t` title) matched with OR semantics, as PR-Agent does, plus a per-run `finding_id`. Line numbers are excluded. One function is shared by measurement, dedupe and the bench adjudication overlay.
8. **Category enum.** The output notes' 11-value enum is canonical (it carries the suppression semantics), instead of the measurement notes' Martian-aligned enum. Martian categories map onto it in `docs/BENCHMARKS.md`. Hypothesis categories use the same enum.
9. **"Did the code change" check.** One `region_touched` helper and one batched judge (at most 10 findings per call, at most 50 judged per PR) serve both later-run acknowledgement and close-time outcomes.
10. **Webhook-triggered follow-up work** (outcome evaluation on close, metadata re-check on edit) is allowed alongside ADR-0012 and recorded in a new ADR. GitHub `pull_request` payload support is added once, by MEAS-09, and reused by RULE-14.
11. **Forgejo poller.** Only MEAS-02 changes `poll_pr`. It is a standalone defect fix, because inline-thread replies are never seen today.
12. **Bot identity** comes from `ReviewHost::bot_login()`. Hosts the gateway builds are seeded with the identity resolved at startup, so `AR_BOT_LOGIN` still wins. A shared `latest_bot_review_state` helper is moved from `ar-chat` to `ar-review` by RULE-02.
13. **Scope keys.** Learnings are scoped by lowercased `owner/repo` with no provider, so mirrored repositories share learnings. Findings and feedback vectors carry the provider. `PrKey` stays without a provider (see Unresolved).
14. **Learnings.** Legacy unscoped rows are excluded from retrieval until assigned. `forget` is scoped to the repository (SEC-06). `remember`, `forget` and the lifecycle commands are gated to approvers (LEARN-08, maintainer decision); a non-approver's `remember` becomes a candidate. `require_approval` defaults to false. Inferred rules always need approval.
15. **Feedback suppression** stores its corpus in the findings store instead of a separate `feedback_vectors` table and Dynamo table. The learning notes' separate DynamoDB store is merged into MEAS-20.
16. **`AGENTS.md` and `.claude/rules`** are edited only by the factory-class items CLEAN-04 and CLEAN-05.
17. **Co-change graph and LLM triage** are deleted, not wired.
18. **Docstring command** keeps a retirement tombstone indefinitely.
19. **Effort** lives under `review.effort` (not top-level `effort`), and a repository may set `review.cost_cap_usd`.
20. **Threat IDs.** T3a (PR config), T3b (guideline, agent-config and asset injection), T4a (credential reads; the notes' "T4b" is renumbered because no T4a exists), T6b (suppression poisoning) and T10 (local review).
21. **Issue #1** is rewritten as a tracking issue for RULE-12 to RULE-14. **Issue #2** stays a separate feature (line-anchored findings, unlike checks) and is rewritten to point at RULE-22 and RULE-23.
22. **Deferred:** PR-scoped learnings until merge (`origin_pr` is stored now), OCR of images, per-agent separate passes for issue #2, Jira and Linear.

Decisions made on 2026-11-16 to make the human-gated items agent-runnable:

23. **GitHub check runs** ([#141](https://github.com/jwilger/auto_review/issues/141)) are built now and on by default (`AR_GITHUB_CHECK_RUNS=true`). The maintainer adds `Checks: Read & write` to the production App; a 403 falls back to the per-check commit status.
24. **GitHub live verification** ([#142](https://github.com/jwilger/auto_review/issues/142)) runs on the sandbox repository `jwilger/auto-review-sandbox`. The agent forces reviews with `@auto-review re-review` and observes results only through the GitHub API, and never changes App settings. auto_review is self-hosted, so there is no shared App: each operator creates their own per docs/GITHUB.md, and #141/#142 add `Checks: Read & write` and the Pull request event subscription to that documented setup. The maintainer has no GitHub deployment yet, so [#161](https://github.com/jwilger/auto_review/issues/161) (not ready, maintainer) creates the App and gateway, the sandbox repository and its installation, and [#142](https://github.com/jwilger/auto_review/issues/142) is blocked by it.
25. **LLM defaults and agent access.** Every LLM setting gets a local-Ollama default in auto_review itself ([#160](https://github.com/jwilger/auto_review/issues/160)): `LLM_BASE_URL=http://127.0.0.1:11434`, no API key, `LLM_REASONING_MODEL=qwen2.5-coder:32b`, `LLM_CHEAP_MODEL=qwen2.5-coder:7b`, `LLM_EMBEDDING_MODEL=bge-m3`. The gateway stops requiring `LLM_BASE_URL`, the Cheap and Embedding tiers turn on by default, and `off` disables a tier. Factory agents use the same defaults; `$HOME/.config/auto_review/agent-llm.env` is optional and only overrides them (values are never printed or committed). `AR_STATS_METRICS_URL` defaults to `http://127.0.0.1:8080/metrics`. An unreachable endpoint or missing model blocks the item for the maintainer.
26. **Bench rules G1–G3** ([#144](https://github.com/jwilger/auto_review/issues/144), [#145](https://github.com/jwilger/auto_review/issues/145), [#146](https://github.com/jwilger/auto_review/issues/146)) are run and applied by an agent exactly as written, with 3-run aggregates posted on the issue. G2's cost limit is a mean of $0.50 per review. The 2-week opt-in guard on G2 is dropped; the addressed-rate metric shows any regression after the flip.
27. **Feedback suppression default** ([#147](https://github.com/jwilger/auto_review/issues/147)) is decided by rule: `enforce` becomes the default when at least 50 evaluated would-suppress findings exist and their addressed rate is at most 10%, read from `auto_review_feedback_shadow_outcomes_total` ([#158](https://github.com/jwilger/auto_review/issues/158)) at `AR_STATS_METRICS_URL`. With fewer findings the item blocks as "insufficient shadow data".
28. **MCP server** ([#148](https://github.com/jwilger/auto_review/issues/148)) is built now, after local review.
29. **Forgejo verification is split.** [#143](https://github.com/jwilger/auto_review/issues/143) verifies v17 behavior on a throwaway container from the E2E runbook, with a gateway built from the branch. [#157](https://github.com/jwilger/auto_review/issues/157) holds the v18 suggestion, reply and resolve checks and stays not ready until Forgejo v18 is released.

## Unverified and unresolved

- Whether a non-admin Forgejo token may dismiss reviews, and whether GitHub's "restrict who can dismiss" blocks Apps ([#142](https://github.com/jwilger/auto_review/issues/142), [#143](https://github.com/jwilger/auto_review/issues/143)).
- The GitHub App permission needed for `resolveReviewThread` and for review-comment reactions. The App must also be subscribed to Pull request events ([#142](https://github.com/jwilger/auto_review/issues/142)).
- Whether Forgejo creates a new review record per inline thread reply (MEAS-02), renders `<details>` in reviews, treats `warning` as satisfying a required status, and supports `pull_request: types: [closed]` in Actions ([#143](https://github.com/jwilger/auto_review/issues/143)).
- The Forgejo v18 release date. The suggestion, reply and resolve paths are written against main's swagger and verified by [#157](https://github.com/jwilger/auto_review/issues/157) after release.
- Whether OpenAI-compatible backends enforce `maxLength` under strict mode (so lengths are enforced in Rust).
- No vendor publishes a suppression threshold, rule-retirement thresholds or agent turn budgets. All of them are defaults tuned in shadow mode or on the bench.
- **Unresolved:** `PrKey` has no provider, so the same `owner/repo` on Forgejo and GitHub share review history. It needs a maintainer decision.

## All items

| Key | Issue | Title | Ready | Blocked by |
|---|---|---|---|---|
| SEC-01 | [#34](https://github.com/jwilger/auto_review/issues/34) | fix(review): remove the clone credential from the workspace git config | yes | — |
| SEC-02 | [#35](https://github.com/jwilger/auto_review/issues/35) | fix(review): deny credential files and .git to workspace tools | yes | SEC-01 |
| SEC-03 | [#36](https://github.com/jwilger/auto_review/issues/36) | fix(review): read repo config from the PR base branch | yes | — |
| SEC-04 | [#37](https://github.com/jwilger/auto_review/issues/37) | fix(prompts): fence untrusted text in review prompts | yes | — |
| SEC-05 | [#38](https://github.com/jwilger/auto_review/issues/38) | fix(chat): only override approvers can obtain an approval through chat | yes | SEC-03 |
| SEC-06 | [#39](https://github.com/jwilger/auto_review/issues/39) | fix(index): scope learnings and chat forget to the repository | yes | — |
| SEC-07 | [#40](https://github.com/jwilger/auto_review/issues/40) | fix(index): key the vector store by repository | yes | SEC-06 |
| SEC-08 | [#41](https://github.com/jwilger/auto_review/issues/41) | feat(review): withhold credential-file contents from review prompts | yes | SEC-02 |
| SEC-09 | [#42](https://github.com/jwilger/auto_review/issues/42) | feat(review): note when a PR changes .auto_review.yaml | yes | SEC-03 |
| SEC-10 | [#43](https://github.com/jwilger/auto_review/issues/43) | feat(cli): manage unscoped legacy learnings | yes | SEC-06 |
| OUT-01 | [#44](https://github.com/jwilger/auto_review/issues/44) | feat(forge): report host capabilities and the bot identity | yes | — |
| RULE-01 | [#45](https://github.com/jwilger/auto_review/issues/45) | feat(review): compute a deterministic merge recommendation | yes | SEC-03, OUT-01 |
| RULE-02 | [#46](https://github.com/jwilger/auto_review/issues/46) | feat(forge): dismiss the bot's stale change request | yes | OUT-01 |
| RULE-03 | [#47](https://github.com/jwilger/auto_review/issues/47) | feat(review): make approval an opt-in policy with shadow mode as the default | yes | RULE-01, RULE-02, SEC-05 |
| MEAS-01 | [#48](https://github.com/jwilger/auto_review/issues/48) | test(gateway): pin Prometheus rules and Grafana dashboard to exposed metrics | yes | — |
| MEAS-02 | [#49](https://github.com/jwilger/auto_review/issues/49) | fix(gateway): poll Forgejo inline review-thread replies for mentions | yes | — |
| OUT-02 | [#50](https://github.com/jwilger/auto_review/issues/50) | feat(prompts): add category and title to review findings | yes | — |
| MEAS-03 | [#51](https://github.com/jwilger/auto_review/issues/51) | feat(review): give each posted finding a stable id and fingerprint marker | yes | OUT-02 |
| MEAS-04 | [#52](https://github.com/jwilger/auto_review/issues/52) | feat(index): persist posted findings and review runs in a findings store | yes | MEAS-03 |
| MEAS-05 | [#53](https://github.com/jwilger/auto_review/issues/53) | feat(gateway): export review cost, findings per review and findings by severity and category | yes | MEAS-01, OUT-02 |
| OUT-03 | [#54](https://github.com/jwilger/auto_review/issues/54) | feat(forge): list inline review comments with anchors | yes | OUT-01 |
| MEAS-06 | [#55](https://github.com/jwilger/auto_review/issues/55) | feat(review): bind posted inline comment ids to stored findings | yes | MEAS-04, OUT-03 |
| MEAS-07 | [#56](https://github.com/jwilger/auto_review/issues/56) | feat(orchestrator): classify untouched findings as not addressed from the final diff | yes | MEAS-04 |
| MEAS-08 | [#57](https://github.com/jwilger/auto_review/issues/57) | feat(review): judge touched findings with the cheap model | yes | MEAS-07, SEC-04 |
| MEAS-09 | [#58](https://github.com/jwilger/auto_review/issues/58) | feat(gateway): evaluate finding outcomes when a pull request closes | yes | MEAS-08 |
| MEAS-10 | [#59](https://github.com/jwilger/auto_review/issues/59) | feat(chat): record addressed, won't-fix and incorrect feedback on findings | yes | MEAS-04 |
| MEAS-11 | [#60](https://github.com/jwilger/auto_review/issues/60) | feat(gateway): export finding outcome and feedback metrics with an addressed-rate rule | yes | MEAS-09, MEAS-10, MEAS-05 |
| MEAS-12 | [#61](https://github.com/jwilger/auto_review/issues/61) | refactor(review): extract a host-free review core from review_pull_request | yes | — |
| MEAS-13 | [#62](https://github.com/jwilger/auto_review/issues/62) | test(bench): run bench fixtures through the review core with workspace snapshots and cost | yes | MEAS-12 |
| MEAS-14 | [#63](https://github.com/jwilger/auto_review/issues/63) | test(bench): add cross-file labelled fixtures and a findings-replay mode | yes | MEAS-13 |
| MEAS-15 | [#64](https://github.com/jwilger/auto_review/issues/64) | feat(cli): import the Martian Code Review Bench offline set on demand | yes | MEAS-13 |
| MEAS-16 | [#65](https://github.com/jwilger/auto_review/issues/65) | feat(cli): score benchmark fixtures with an LLM judge and Martian profiles | yes | MEAS-15, OUT-02 |
| MEAS-17 | [#66](https://github.com/jwilger/auto_review/issues/66) | feat(chat): infer the target finding from the reply thread | yes | MEAS-10, MEAS-06, MEAS-02 |
| MEAS-18 | [#67](https://github.com/jwilger/auto_review/issues/67) | feat(forge): read emoji reactions on findings at evaluation time | yes | MEAS-06, MEAS-09 |
| MEAS-19 | [#68](https://github.com/jwilger/auto_review/issues/68) | feat(cli): add an auto-review stats report over the findings store | yes | MEAS-08, MEAS-10, MEAS-18 |
| MEAS-20 | [#69](https://github.com/jwilger/auto_review/issues/69) | feat(index): add a DynamoDB findings store for AgentCore | yes | MEAS-04 |
| MEAS-21 | [#70](https://github.com/jwilger/auto_review/issues/70) | feat(agentcore): accept outcome-evaluation invocations on pull request close | yes | MEAS-09, MEAS-20 |
| MEAS-22 | [#71](https://github.com/jwilger/auto_review/issues/71) | feat(cli): adjudicate unlisted benchmark findings instead of counting them as false positives | yes | MEAS-16, MEAS-03 |
| CLEAN-01 | [#72](https://github.com/jwilger/auto_review/issues/72) | fix(chat): use host-neutral wording in chat prompts and replies | yes | SEC-04 |
| CLEAN-02 | [#73](https://github.com/jwilger/auto_review/issues/73) | feat(chat): retire the docstring command | yes | — |
| CLEAN-03 | [#74](https://github.com/jwilger/auto_review/issues/74) | chore: remove the unused co-change graph and LLM triage modules | yes | — |
| CLEAN-04 | [#75](https://github.com/jwilger/auto_review/issues/75) | chore(factory): sync AGENTS.md with the retired docstring command and removed modules | yes | CLEAN-02, CLEAN-03 |
| OUT-04 | [#76](https://github.com/jwilger/auto_review/issues/76) | feat(review): suppress unwanted finding categories by repo config | yes | SEC-03, OUT-02 |
| OUT-05 | [#77](https://github.com/jwilger/auto_review/issues/77) | feat(review): cap inline comments per review and move the overflow into the summary | yes | OUT-04 |
| OUT-06 | [#78](https://github.com/jwilger/auto_review/issues/78) | feat(prompts): keep finding titles and messages concise | yes | OUT-02 |
| OUT-07 | [#79](https://github.com/jwilger/auto_review/issues/79) | feat(review): add review profiles quiet, standard and thorough | yes | OUT-05, SEC-03 |
| OUT-08 | [#80](https://github.com/jwilger/auto_review/issues/80) | feat(review): post multi-line findings as multi-line review comments | yes | OUT-01 |
| OUT-09 | [#81](https://github.com/jwilger/auto_review/issues/81) | feat(review): attach committable suggestion blocks to concrete findings | yes | OUT-08, OUT-05 |
| OUT-10 | [#82](https://github.com/jwilger/auto_review/issues/82) | feat(review): show file:line evidence for why a finding was flagged | yes | OUT-02, SEC-02 |
| OUT-11 | [#83](https://github.com/jwilger/auto_review/issues/83) | feat(review): add Prompt for AI agents blocks to findings and the summary | yes | OUT-09, OUT-05, SEC-04 |
| OUT-12 | [#84](https://github.com/jwilger/auto_review/issues/84) | feat(review): skip re-posting findings that are still open | yes | MEAS-03, MEAS-04, OUT-03, OUT-05 |
| OUT-13 | [#85](https://github.com/jwilger/auto_review/issues/85) | feat(forge): edit issue and review comments | yes | — |
| OUT-14 | [#86](https://github.com/jwilger/auto_review/issues/86) | feat(review): keep one persistent summary comment per PR and edit it in place | yes | OUT-13, OUT-01 |
| OUT-15 | [#87](https://github.com/jwilger/auto_review/issues/87) | feat(review): skip posting a new review when nothing changed for reviewers | yes | OUT-14, RULE-02, RULE-03 |
| OUT-16 | [#88](https://github.com/jwilger/auto_review/issues/88) | feat(review): acknowledge findings addressed by later commits | yes | OUT-12, OUT-13, MEAS-06, MEAS-08 |
| OUT-17 | [#89](https://github.com/jwilger/auto_review/issues/89) | feat(review): reply to and resolve addressed threads where the host allows it | yes | OUT-16, OUT-01 |
| OUT-18 | [#90](https://github.com/jwilger/auto_review/issues/90) | feat(review): put a verdict line and noise counts at the top of the summary | yes | OUT-14, OUT-05, OUT-12, RULE-03 |
| OUT-19 | [#91](https://github.com/jwilger/auto_review/issues/91) | feat(review): add a collapsed per-file change table to the summary | yes | OUT-14 |
| OUT-20 | [#92](https://github.com/jwilger/auto_review/issues/92) | feat(prompts): show a review-effort estimate in the summary | yes | OUT-14 |
| OUT-21 | [#93](https://github.com/jwilger/auto_review/issues/93) | feat(gateway): count suppressed, deduplicated and addressed findings | yes | MEAS-05, OUT-04, OUT-05, OUT-06, OUT-12, OUT-15, OUT-16, OUT-17 |
| RULE-04 | [#94](https://github.com/jwilger/auto_review/issues/94) | feat(review): include nearest AGENTS.md and CLAUDE.md from the base branch | yes | SEC-01, SEC-03, SEC-04 |
| RULE-05 | [#95](https://github.com/jwilger/auto_review/issues/95) | feat(review): honor REVIEW.md and .cursor/BUGBOT.md review instructions | yes | RULE-04 |
| RULE-06 | [#96](https://github.com/jwilger/auto_review/issues/96) | feat(review): apply Copilot instruction files with applyTo scoping | yes | RULE-04 |
| RULE-07 | [#97](https://github.com/jwilger/auto_review/issues/97) | feat(review): extract Code Review Rules sections from AGENTS.md | yes | RULE-04 |
| RULE-08 | [#98](https://github.com/jwilger/auto_review/issues/98) | feat(config): add path_instructions to .auto_review.yaml | yes | RULE-04 |
| RULE-09 | [#99](https://github.com/jwilger/auto_review/issues/99) | feat(review): list the guidelines and learnings applied in the summary | yes | RULE-04, OUT-14, OUT-07, SEC-06 |
| RULE-10 | [#100](https://github.com/jwilger/auto_review/issues/100) | feat(chat): honor approval mode on for chat approval requests | yes | RULE-03, SEC-05 |
| RULE-11 | [#101](https://github.com/jwilger/auto_review/issues/101) | feat(gateway): count approval decisions | yes | RULE-03, MEAS-05 |
| RULE-12 | [#102](https://github.com/jwilger/auto_review/issues/102) | feat(review): re-check PR metadata without a full re-review | yes | SEC-03, RULE-02, RULE-03 |
| RULE-13 | [#103](https://github.com/jwilger/auto_review/issues/103) | feat(gateway): re-check metadata when a Forgejo PR title or body is edited | yes | RULE-12, MEAS-09 |
| RULE-14 | [#104](https://github.com/jwilger/auto_review/issues/104) | feat(gateway): re-check metadata when a GitHub PR title or body is edited | yes | RULE-12, MEAS-09 |
| RULE-15 | [#105](https://github.com/jwilger/auto_review/issues/105) | feat(forge): fetch issues through ReviewHost on Forgejo and GitHub | yes | — |
| RULE-16 | [#106](https://github.com/jwilger/auto_review/issues/106) | feat(review): add linked issues to the review prompt | yes | RULE-15, SEC-03, SEC-04 |
| AGENT-01 | [#107](https://github.com/jwilger/auto_review/issues/107) | refactor(review): extract a reusable bounded agent loop and shared LLM test fakes | yes | — |
| AGENT-02 | [#108](https://github.com/jwilger/auto_review/issues/108) | feat(review): add list_dir and find_symbol workspace tools with untrusted framing and an output budget | yes | AGENT-01, SEC-02, SEC-04 |
| AGENT-03 | [#109](https://github.com/jwilger/auto_review/issues/109) | feat(index): extract call sites with tree-sitter and expose find_callers and find_references tools | yes | AGENT-02 |
| AGENT-04 | [#110](https://github.com/jwilger/auto_review/issues/110) | feat(llm): enforce a per-review cost budget with a soft limit | yes | AGENT-01 |
| AGENT-05 | [#111](https://github.com/jwilger/auto_review/issues/111) | feat(review): post verified findings with a budget-exhausted note when the cost cap is hit | yes | AGENT-04, RULE-03 |
| AGENT-06 | [#112](https://github.com/jwilger/auto_review/issues/112) | feat(review): run agentic review generation behind AR_AGENTIC_GENERATION_TURNS | yes | AGENT-02, AGENT-04, MEAS-12, OUT-10 |
| LEARN-01 | [#113](https://github.com/jwilger/auto_review/issues/113) | feat(index): store finding embeddings in the findings store | yes | MEAS-04, MEAS-20, MEAS-10, OUT-02 |
| LEARN-02 | [#114](https://github.com/jwilger/auto_review/issues/114) | feat(review): run feedback suppression in shadow mode with similarity logging | yes | LEARN-01, MEAS-05, SEC-03 |
| LEARN-03 | [#115](https://github.com/jwilger/auto_review/issues/115) | feat(review): suppress findings that match repeatedly dismissed feedback | yes | LEARN-02, OUT-14 |
| RULE-17 | [#116](https://github.com/jwilger/auto_review/issues/116) | feat(review): evaluate custom natural-language pre-merge checks | yes | SEC-03, SEC-04, MEAS-05, RULE-03 |
| RULE-18 | [#117](https://github.com/jwilger/auto_review/issues/117) | feat(review): post one commit status per pre-merge check | yes | RULE-17 |
| RULE-19 | [#118](https://github.com/jwilger/auto_review/issues/118) | refactor(review): run the PR metadata gate as a built-in pre-merge check | yes | RULE-18, RULE-12 |
| RULE-20 | [#119](https://github.com/jwilger/auto_review/issues/119) | feat(review): assess linked-issue acceptance criteria as a pre-merge check | yes | RULE-16, RULE-18 |
| RULE-21 | [#120](https://github.com/jwilger/auto_review/issues/120) | feat(chat): let override approvers waive a failing pre-merge check | yes | RULE-18, SEC-05 |
| RULE-22 | [#121](https://github.com/jwilger/auto_review/issues/121) | feat(config): define structured review agents with checklists | yes | SEC-03, SEC-04 |
| RULE-23 | [#122](https://github.com/jwilger/auto_review/issues/122) | feat(review): attribute findings to the review agent that raised them | yes | RULE-22, OUT-02 |
| RULE-24 | [#123](https://github.com/jwilger/auto_review/issues/123) | feat(review): flag newly added images and binary files for human inspection | yes | SEC-04, RULE-01 |
| RULE-25 | [#124](https://github.com/jwilger/auto_review/issues/124) | feat(review): flag instruction-like text in agent-config and docs files | yes | RULE-24 |
| RULE-26 | [#125](https://github.com/jwilger/auto_review/issues/125) | feat(prompts): add a security advisory when agent-config files change | yes | RULE-25 |
| AGENT-07 | [#126](https://github.com/jwilger/auto_review/issues/126) | feat(review): add effort tiers quick, standard and deep | yes | AGENT-05, AGENT-06, SEC-03, OUT-07 |
| AGENT-08 | [#127](https://github.com/jwilger/auto_review/issues/127) | feat(chat): accept an effort argument on re-review | yes | AGENT-07 |
| LEARN-04 | [#128](https://github.com/jwilger/auto_review/issues/128) | feat(cli): review local changes with auto-review review local | yes | MEAS-12, SEC-02 |
| LEARN-05 | [#129](https://github.com/jwilger/auto_review/issues/129) | feat(cli): add JSON output and severity-gate exit codes to local review | yes | LEARN-04 |
| LEARN-06 | [#130](https://github.com/jwilger/auto_review/issues/130) | feat(cli): add repo context and agentic verification to local review, limited to tracked files | yes | LEARN-04, SEC-02, SEC-06 |
| AGENT-09 | [#131](https://github.com/jwilger/auto_review/issues/131) | feat(llm): cap in-flight LLM requests per process | yes | — |
| AGENT-10 | [#132](https://github.com/jwilger/auto_review/issues/132) | perf(review): verify findings concurrently in the agentic verifier | yes | AGENT-01, AGENT-09 |
| AGENT-11 | [#133](https://github.com/jwilger/auto_review/issues/133) | feat(review): add a hypothesis planner and parallel investigators in shadow mode | yes | AGENT-07, AGENT-10 |
| AGENT-12 | [#134](https://github.com/jwilger/auto_review/issues/134) | feat(review): merge, dedupe and rank investigator findings into deep reviews | yes | AGENT-11 |
| LEARN-07 | [#135](https://github.com/jwilger/auto_review/issues/135) | feat(index): give learnings a lifecycle status and provenance | yes | SEC-06 |
| LEARN-08 | [#136](https://github.com/jwilger/auto_review/issues/136) | feat(chat): gate remember, forget and learning lifecycle commands behind approvers | yes | LEARN-07, SEC-03 |
| LEARN-09 | [#137](https://github.com/jwilger/auto_review/issues/137) | feat(review): attribute findings to learnings and track per-rule acceptance | yes | LEARN-07, MEAS-04, MEAS-10 |
| LEARN-10 | [#138](https://github.com/jwilger/auto_review/issues/138) | feat(review): draft candidate rules from repeatedly dismissed findings | yes | LEARN-01, LEARN-08, MEAS-10 |
| LEARN-11 | [#139](https://github.com/jwilger/auto_review/issues/139) | feat(review): auto-disable learned rules that stop helping | yes | LEARN-09, LEARN-10 |
| CLEAN-05 | [#140](https://github.com/jwilger/auto_review/issues/140) | chore(factory): update AGENTS.md and path rules for the new review stages | yes | AGENT-06, OUT-14, LEARN-03, RULE-17, MEAS-09, RULE-04, AGENT-11 |
| RULE-27 | [#141](https://github.com/jwilger/auto_review/issues/141) | feat(github): publish pre-merge checks as check runs | yes | RULE-18 |
| CLEAN-06 | [#142](https://github.com/jwilger/auto_review/issues/142) | chore(ops): verify GitHub App behavior live on the sandbox repository | yes | MEAS-09, RULE-14, RULE-02, OUT-17, MEAS-18, CLEAN-10 |
| CLEAN-07 | [#143](https://github.com/jwilger/auto_review/issues/143) | chore(ops): verify Forgejo v17 host behavior on a throwaway instance | yes | MEAS-02, RULE-02, RULE-18, MEAS-21, OUT-17, LEARN-03, OUT-13 |
| CLEAN-08 | [#157](https://github.com/jwilger/auto_review/issues/157) | chore(ops): verify Forgejo v18 suggestion, reply and resolve paths live | no | OUT-09, OUT-17, CLEAN-07 |
| CLEAN-09 | [#160](https://github.com/jwilger/auto_review/issues/160) | feat(config): default every LLM setting to a local Ollama setup | yes | — |
| CLEAN-10 | [#161](https://github.com/jwilger/auto_review/issues/161) | chore(ops): deploy a GitHub App and gateway for the sandbox repository | no | — |
| AGENT-13 | [#144](https://github.com/jwilger/auto_review/issues/144) | chore(review): make the agentic verifier the default for standard effort if bench rule G1 passes | yes | MEAS-14, AGENT-07, AGENT-10 |
| AGENT-14 | [#145](https://github.com/jwilger/auto_review/issues/145) | chore(review): enable agentic generation for standard effort if bench rule G2 passes | yes | MEAS-14, AGENT-06, AGENT-07, SEC-02 |
| AGENT-15 | [#146](https://github.com/jwilger/auto_review/issues/146) | chore(review): post fan-out findings for deep effort if bench rule G3 passes | yes | MEAS-14, AGENT-12 |
| LEARN-14 | [#158](https://github.com/jwilger/auto_review/issues/158) | feat(gateway): export outcomes of findings shadow suppression would have hidden | yes | LEARN-02, MEAS-09, MEAS-11 |
| LEARN-12 | [#147](https://github.com/jwilger/auto_review/issues/147) | chore(review): make feedback suppression enforce by default if shadow outcomes pass the rule | yes | LEARN-03, LEARN-14 |
| LEARN-13 | [#148](https://github.com/jwilger/auto_review/issues/148) | feat(cli): add an MCP stdio server exposing review_diff | yes | LEARN-05, LEARN-06 |
| SEC-EPIC | [#149](https://github.com/jwilger/auto_review/issues/149) | chore: track trust and security fixes | no | — |
| MEAS-EPIC | [#150](https://github.com/jwilger/auto_review/issues/150) | chore: track measurement and benchmark work | no | — |
| OUT-EPIC | [#151](https://github.com/jwilger/auto_review/issues/151) | chore: track review output and noise work | no | — |
| RULE-EPIC | [#152](https://github.com/jwilger/auto_review/issues/152) | chore: track rules, requirements and gating work | no | — |
| AGENT-EPIC | [#153](https://github.com/jwilger/auto_review/issues/153) | chore: track agentic review, effort and cost work | no | — |
| LEARN-EPIC | [#154](https://github.com/jwilger/auto_review/issues/154) | chore: track learning loop and local review work | no | — |
| CLEAN-EPIC | [#155](https://github.com/jwilger/auto_review/issues/155) | chore: track cleanup, drift fixes and live verification | no | — |
