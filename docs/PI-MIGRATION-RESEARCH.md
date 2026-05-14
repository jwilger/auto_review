# Pi Migration Research for `auto_review`

Date: 2026-05-14

## Executive summary

`auto_review`'s Kilo setup is not just instructions. It combines always-loaded guardrails, specialist agents, slash-command workflows, on-demand skills, Forgejo MCP access, Rust LSP configuration, path permissions, and project-local plugins that enforce parts of the RED-GREEN-REFACTOR (RGR), Forgejo, and toolchain discipline.

Pi can replicate most of this with native project resources and a small project extension:

- Keep `AGENTS.md`: Pi loads `AGENTS.md` automatically from parent/current directories.
- Move `.kilo/skills/*` to `.pi/skills/*` with minimal changes.
- Move `.kilo/command/*.md` to `.pi/prompts/*.md`, except workflows that should mutate/check session state should become extension commands.
- Move `.kilo/agent/*.md` to `.pi/agents/*.md` and use `@tintinweb/pi-subagents` for Claude/Kilo-style subagents.
- Use `pi-mcp-adapter` plus `.mcp.json` for the Forgejo MCP server.
- Use `pi-lens` for LSP/diagnostics/AST-grep/read-before-edit/secrets checks, but override Rust LSP to run through `nix develop`.
- Use `@gotgenes/pi-permission-system` for deterministic allow/ask/deny policy over tools, bash, MCP, skills, and external directories.
- Create one project-local Pi extension, tentatively `.pi/extensions/auto-review-guardrails.ts`, to port Kilo's custom plugin behavior and make several guardrails stricter than Kilo could.

Recommended initial package set:

```jsonc
{
  "packages": [
    "npm:context-mode@1.0.131",
    "npm:pi-mcp-adapter@2.6.1",
    "npm:pi-lens@3.8.44",
    "npm:@gotgenes/pi-permission-system@5.16.0",
    "npm:@tintinweb/pi-subagents@0.7.2",
  ],
}
```

Defer `pi-agent-flow` and `pi-teams` until after the basic Kilo parity layer is stable; they are useful but introduce a second orchestration model.

## Current Kilo setup inventory

### `kilo.json`

`kilo.json` currently defines:

- Instructions: `AGENTS.md` plus `.kilo/rules/*.md`.
- Default agent: `auto-review-rust-implementer`.
- Plugin: `kilo-rtk@npm:@jwilger/kilo-rtk`.
- Forgejo MCP: local `forgejo-mcp` command against `https://git.johnwilger.com`, using `FORGEJO_TOKEN` from the environment.
- Rust LSP: `nix develop . --command rust-analyzer`, all features, clippy check command.
- Permissions: broad read/search/list/task/skill/LSP/bash/edit allow, with hard denies for `.env`, `.env.*`, `*.key`, `*.pem`, asks for secret/credential paths, external directories ask, and `forgejo_*` allowed.
- Compaction: auto/prune enabled.
- Snapshot: enabled.
- Share: manual.

### Rules

`.kilo/rules` contains seven always-loaded guardrails:

- `branch-first.md`: create/switch to a PR branch before implementation.
- `forgejo.md`: Forgejo, not GitHub; inline feedback must be answered in-thread with `new_position = comment.position` and `old_position = 0`.
- `scope-hygiene.md`: stage explicit paths only; no `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
- `security.md`: do not read/commit secrets; threat-model and red-team coupling.
- `tdd-discipline.md`: behavior production code requires observed RED first; specialist RGR handoffs.
- `toolchain.md`: Nix-pinned Rust; no system `rustup`; focused gates.
- `verification.md`: narrow verification first, broader feasible gate before handoff.

Most of these are duplicated or summarized in `AGENTS.md`, but branch-first and scope-hygiene should remain explicit in Pi via prompt text plus deterministic hooks.

### Agents

Kilo agents:

- `auto-review-rust-implementer`: primary implementation agent; orchestrates specialist RGR agents.
- `rgr-test-author`: edit-capable RED test author.
- `rgr-test-reviewer`: read-only RED reviewer.
- `rgr-diagnostic-implementer`: edit-capable minimal GREEN implementer for one diagnostic.
- `rgr-implementation-reviewer`: read-only GREEN reviewer.
- `architecture-reviewer`: read-only architecture/public-surface/observability reviewer.
- `test-coverage-reviewer`: read-only coverage/RGR evidence reviewer.
- `security-reviewer`: read-only security/threat-model reviewer.
- `docs-operator-reviewer`: read-only docs/deployment/operator reviewer.
- `forgejo-feedback-processor`: PR feedback reflection/classification/remediation agent.

### Commands

Kilo command workflows:

- `bugfix-rgr`: reproduce a defect with RED before fixing.
- `outside-in-rgr`: fine-grained outside-in RGR microcycle with specialist agents.
- `tdd-implement`: explicit RED-GREEN-REFACTOR cycle.
- `refactor-safely`: green baseline, small refactor, rerun focused tests.
- `verify`: focused/full repository verification through Nix-pinned toolchain.
- `local-review`: branch/diff review with architecture, coverage, security reviewers.
- `local-review-uncommitted`: working-tree review.
- `prepare-forgejo-pr`: scope audit, explicit staging, conventional commit, verification, `tea pr create`.
- `process-pr-feedback`: reflect/classify/remediate/reply inline.

### Skills

Kilo skills are already close to Pi skill format:

- `forgejo-feedback-protocol`
- `outside-in-rgr-microcycle`
- `outside-in-tdd`
- `review-taxonomy`
- `rgr-plan-structure`
- `rust-workspace-engineering`
- `security-threat-model`

These should copy directly to `.pi/skills/` because Pi supports `SKILL.md` directories and `/skill:<name>` commands.

### Project-local Kilo plugins

The `.kilo/plugin` TypeScript files implement the important enforceable behavior:

1. `auto-review-discipline.ts`
   - Registers tools: `rgr_start`, `rgr_record_red`, `rgr_mark_green`, `rgr_mark_refactor`, `rgr_status`.
   - Blocks edits/writes/apply_patch to `crates/*/src/*.rs` unless the active session has recorded observed RED output.
   - Tracks touched files.
   - Blocks component-waterfall todo lists that do not mention RED/failing tests/RGR.
   - Injects active RGR/session context during compaction.

2. `auto-review-toolchain.ts`
   - Sets `CARGO_HOME=$worktree/.dependencies/cargo` and `RUSTUP_HOME=$worktree/.dependencies/rustup` for shell commands.
   - Blocks unsafe commands: `rustup`, broad `git add`, `git commit -a`, `--no-verify`, `--no-gpg-sign`, `git reset --hard`, `git checkout --`, and force push.

3. `auto-review-forgejo.ts`
   - Registers `forgejo_inline_reply_payload`, `forgejo_feedback_status`, and `forgejo_review_api_recipe`.
   - Blocks top-level Forgejo/GitHub PR comment paths when inline-thread replies are required.

4. `auto-review-context.ts`
   - Preserves active RGR/touched-file/verification/Forgejo-feedback context during compaction.
   - Marks RGR/Forgejo tool results as context-preserved metadata.

Kilo plugin state is held in in-memory maps and surfaced during compaction. Pi's session event store can make this more durable.

## Pi capabilities relevant to the migration

### Built-in project resource model

Pi loads:

- `AGENTS.md` or `CLAUDE.md` as context files from global, parent, and current directories.
- `.pi/SYSTEM.md` to replace the system prompt, or `.pi/APPEND_SYSTEM.md` to append to it.
- Project settings from `.pi/settings.json`.
- Project extensions from `.pi/extensions/`.
- Project skills from `.pi/skills/`.
- Project prompt templates from `.pi/prompts/`.
- Project themes from `.pi/themes/`.
- Pi packages from npm/git/local paths via the `packages` setting.

Project package/resource paths in `.pi/settings.json` resolve relative to `.pi`.

### Extensions

Pi extensions are TypeScript modules. They can:

- `registerTool()` with TypeBox schemas and tool-specific prompt snippets/guidelines.
- `registerCommand()` for slash commands.
- Subscribe to events, including `tool_call`, `before_agent_start`, `context`, `session_start`, `session_before_compact`, and `session_compact`.
- Block tool calls by returning `{ block: true, reason }` from `tool_call`.
- Mutate tool arguments before execution.
- Persist extension state with `appendEntry()` and reconstruct state from session entries.
- Add resource paths during `resources_discover`.

This is enough to port Kilo's plugins and add stricter gates.

### Skills and prompt templates

Pi skills use `SKILL.md` frontmatter with required `name` and `description`; skill commands are available as `/skill:<name>`.

Pi prompt templates are Markdown files invoked as slash commands by filename. They support `$ARGUMENTS`, `$@`, `$1`, `$2`, and slicing such as `${@:2}`. Kilo command Markdown should mostly port directly to `.pi/prompts/`.

### MCP

`pi-mcp-adapter` provides compact MCP access via:

- A proxy `mcp` tool: status, list, search, describe, call, connect, UI messages.
- Optional direct tools per server.
- Lazy/eager/keep-alive lifecycle.
- Project config via `.mcp.json` or Pi-specific `.pi/mcp.json`.

Recommended Forgejo server config should use Nix:

```jsonc
{
  "mcpServers": {
    "forgejo": {
      "command": "sh",
      "args": [
        "-lc",
        "exec nix develop . --command forgejo-mcp --transport stdio --url https://git.johnwilger.com --token \"$FORGEJO_TOKEN\" --user-agent auto_review/forgejo-mcp",
      ],
      "env": {
        "FORGEJO_USER_AGENT": "auto_review/forgejo-mcp",
      },
      "lifecycle": "lazy",
      "idleTimeout": 10,
    },
  },
}
```

### LSP and code intelligence

`pi-lens` provides LSP, diagnostics, formatters, AST-grep rules, read-before-edit guard, secrets scanning, and quality reports. It includes Rust support, but its built-in Rust server prefers `rust-analyzer` on `PATH` and can fall back to a managed download. That conflicts with this project's Nix-only policy.

Use `.pi-lens/lsp.json` to disable the built-in Rust server and add a Nix-backed one:

```jsonc
{
  "disabledServers": ["rust"],
  "servers": {
    "rust-nix": {
      "name": "rust-analyzer via nix develop",
      "extensions": [".rs"],
      "command": "nix",
      "args": ["develop", ".", "--command", "rust-analyzer"],
      "rootMarkers": ["Cargo.toml", "rust-project.json", ".git"],
      "env": {
        "CARGO_HOME": ".dependencies/cargo",
        "RUSTUP_HOME": ".dependencies/rustup",
      },
    },
  },
}
```

### Permission policy

`@gotgenes/pi-permission-system` is the best online match for `kilo.json.permission`. It enforces deterministic gates over tools, bash, MCP, skill, and `external_directory` surfaces. Project config path:

```text
.pi/extensions/pi-permission-system/config.json
```

Example shape:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/gotgenes/pi-permission-system/main/schemas/permissions.schema.json",
  "debugLog": false,
  "permissionReviewLog": true,
  "yoloMode": false,
  "permission": {
    "*": "ask",
    "read": {
      "*": "allow",
      "*.env": "deny",
      "*.env.*": "deny",
      "*.env.example": "allow",
    },
    "write": {
      ".env": "deny",
      ".env.*": "deny",
      "**/*.key": "deny",
      "**/*.pem": "deny",
      "*": "allow",
    },
    "edit": {
      ".env": "deny",
      ".env.*": "deny",
      "**/*.key": "deny",
      "**/*.pem": "deny",
      "*": "allow",
    },
    "bash": {
      "git status *": "allow",
      "git diff *": "allow",
      "nix *": "allow",
      "cargo *": "ask",
      "*": "ask",
    },
    "mcp": {
      "mcp_status": "allow",
      "mcp_list": "allow",
      "forgejo:*": "allow",
      "forgejo_*": "allow",
      "*": "ask",
    },
    "skill": { "*": "allow" },
    "external_directory": "ask",
  },
}
```

The custom project guardrail extension should still block known-bad commands because permission policy is generic; project semantics are more specific.

## Online Pi extension/package scan

Representative packages found on npm with `pi-package`/Pi extension metadata:

| Package                                                | Purpose                                                                                                             | Recommendation for `auto_review`                                                     |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `context-mode@1.0.131`                                 | Context-preserving execution, file analysis, web/document indexing, FTS search                                      | Keep/pin. Already active globally and useful for this repo.                          |
| `pi-mcp-adapter@2.6.1`                                 | MCP proxy/direct tools, lazy server lifecycle, `.mcp.json` support                                                  | Use for Forgejo MCP.                                                                 |
| `pi-lens@3.8.44`                                       | LSP, diagnostics, formatters, AST-grep, read-before-edit, secrets checks                                            | Use, with Nix Rust LSP override.                                                     |
| `@gotgenes/pi-permission-system@5.16.0`                | Deterministic permission policy for tools/bash/MCP/skills/external dirs                                             | Use after source audit; fills Kilo permission gap.                                   |
| `@tintinweb/pi-subagents@0.7.2`                        | Claude-style subagents with `.pi/agents`, tool lists, model/thinking, background execution, result polling/steering | Best match for Kilo specialist agents.                                               |
| `pi-code-nav@0.5.0`                                    | Higher-level exact symbol navigation companion to `pi-lens`                                                         | Optional; useful if LSP navigation UX needs improvement.                             |
| `pi-agent-flow@1.8.39`                                 | Flow-state delegation with sanitized forked context, bundled scout/debug/build/craft/audit flows                    | Defer. Powerful but overlaps with custom RGR and uses `.pi/agents` flow definitions. |
| `pi-teams@0.9.14`                                      | Tmux/Zellij style agent teams and task submission                                                                   | Defer. Better for broad team simulations than strict RGR.                            |
| `pi-zellij@0.4.0` / `pi-zellij-tools@0.1.3`            | Terminal/Zellij integrations, spawn Pi sessions/agents                                                              | Optional local productivity; not required for deterministic parity.                  |
| `pi-web-access@0.10.x` / `@ollama/pi-web-search@0.0.5` | Web search/fetch tools                                                                                              | Optional; current context-mode/web tools may be sufficient.                          |
| `@plannotator/pi-extension@0.19.16`                    | Interactive plan review/annotations                                                                                 | Optional; possibly useful for human plan approval later.                             |
| `@vtstech/pi-security` / `@vtstech/pi-diag`            | Security/diagnostics extensions                                                                                     | Defer pending audit; overlaps with pi-lens and custom threat-model guardrails.       |
| `pi-package-search`, `pi-resource-center`              | Discover/install packages from inside Pi                                                                            | Optional for exploration, not project config.                                        |
| `pi-beads-extension`, `agent-vault`, `pi-prompt-stash` | Task tracking, durable memory, prompt stashing                                                                      | Defer; avoid workflow sprawl until parity layer is stable.                           |
| `pi-bash-confirm`                                      | Extra bash confirmation/notifications                                                                               | Not needed if using permission-system + project guardrails.                          |

A local global package also exists: `@jwilger/pi-orchestrator@0.1.0`, described as deterministic multi-agent SDLC orchestration for Pi. It is not published to npm. It registers `/orchestra` and many `orchestra_*` tools, with workflow/evidence/state concepts. It may be a good future replacement for ad-hoc RGR orchestration, but it currently appears private/local and references older `@mariozechner/*` package names in imports, so it should be treated as a separate hardening project rather than the first migration step.

## Proposed Pi project layout

```text
.pi/
  settings.json
  APPEND_SYSTEM.md                         # optional short always-loaded Pi-only deltas
  prompts/
    bugfix-rgr.md
    outside-in-rgr.md
    tdd-implement.md
    refactor-safely.md
    verify.md
    local-review.md
    local-review-uncommitted.md
    prepare-forgejo-pr.md
    process-pr-feedback.md
  skills/
    forgejo-feedback-protocol/SKILL.md
    outside-in-rgr-microcycle/SKILL.md
    outside-in-tdd/SKILL.md
    review-taxonomy/SKILL.md
    rgr-plan-structure/SKILL.md
    rust-workspace-engineering/SKILL.md
    security-threat-model/SKILL.md
  agents/
    rgr-test-author.md
    rgr-test-reviewer.md
    rgr-diagnostic-implementer.md
    rgr-implementation-reviewer.md
    architecture-reviewer.md
    security-reviewer.md
    test-coverage-reviewer.md
    docs-operator-reviewer.md
    forgejo-feedback-processor.md
  extensions/
    auto-review-guardrails.ts
    auto-review-guardrails.test.ts          # if we add extension tests under a JS/TS test harness
    pi-permission-system/config.json
.mcp.json
.pi-lens/lsp.json
```

Suggested `.pi/settings.json`:

```jsonc
{
  "packages": [
    "npm:context-mode@1.0.131",
    "npm:pi-mcp-adapter@2.6.1",
    "npm:pi-lens@3.8.44",
    "npm:@gotgenes/pi-permission-system@5.16.0",
    "npm:@tintinweb/pi-subagents@0.7.2",
  ],
  "extensions": ["extensions/*.ts"],
  "skills": ["skills"],
  "prompts": ["prompts"],
  "enableSkillCommands": true,
}
```

## Kilo-to-Pi mapping

| Kilo capability                  | Pi replacement                                                                                            |
| -------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `AGENTS.md` loaded by Kilo       | Keep `AGENTS.md`; Pi loads it automatically.                                                              |
| `.kilo/rules/*.md` always loaded | Fold missing deltas into `AGENTS.md` or `.pi/APPEND_SYSTEM.md`; enforce critical ones in extension hooks. |
| `.kilo/skills/*`                 | Copy to `.pi/skills/*`; Pi validates skill frontmatter and exposes `/skill:<name>`.                       |
| `.kilo/command/*.md`             | Copy to `.pi/prompts/*.md`; use extension commands for workflows that need state changes.                 |
| Kilo specialist agents           | `@tintinweb/pi-subagents` with `.pi/agents/*.md`.                                                         |
| Kilo `task` tool                 | Pi `Agent`, `get_subagent_result`, `steer_subagent` from `pi-subagents`.                                  |
| Forgejo MCP in `kilo.json`       | `pi-mcp-adapter` + project `.mcp.json`, lazy lifecycle, Nix command.                                      |
| Rust LSP through Nix             | `pi-lens` custom `.pi-lens/lsp.json` disabling built-in Rust and adding Nix Rust server.                  |
| `kilo.json.permission`           | `@gotgenes/pi-permission-system` project config plus custom guardrail blockers.                           |
| Kilo RGR custom tools            | `auto-review-guardrails.ts` `registerTool()`.                                                             |
| Kilo `tool.execute.before` gates | Pi `tool_call` event handlers.                                                                            |
| Kilo compaction context          | Pi `session_before_compact`/`session_compact`, `before_agent_start`, `appendEntry()`.                     |
| Kilo shell env injection         | Pi `tool_call` mutation for `bash` and/or override `bash` tool with `createBashTool` spawn hook.          |

## Custom extension we should create

Create `.pi/extensions/auto-review-guardrails.ts` with these responsibilities.

### RGR ledger and edit gate

Register tools:

- `rgr_start({ behavior, test, command? })`
- `rgr_record_red({ command, output })`
- `rgr_mark_green({ command, output })`
- `rgr_mark_refactor({ verification })`
- `rgr_status({})`

Enforcement:

- Block `write`/`edit` to production Rust paths (`crates/*/src/*.rs`) until observed RED is recorded.
- Persist cycle events with `appendEntry()` and reconstruct on `session_start`, instead of relying only on in-memory state.
- Track touched files and verification status.
- On `session_before_compact`, inject active RGR state, touched files, and verification status.

Recommended deterministic improvement over Kilo:

- Bind GREEN to the same command recorded for RED unless the agent supplies an explicit reason.
- Store a hash/summary of RED and GREEN output in tool `details`.
- Optionally require `rgr_mark_green` before starting another RGR cycle.
- Optionally maintain an edit budget or require a `rgr_next_diagnostic` call before additional production edits in the same cycle.

### Toolchain and branch gate

Enforcement:

- Set or inject `CARGO_HOME=.dependencies/cargo` and `RUSTUP_HOME=.dependencies/rustup` for bash commands.
- Block raw `rustup`.
- Block `git add .`, `git add -A`, `git add -u`, `git commit -a`, `--no-verify`, `--no-gpg-sign`, `git reset --hard`, `git checkout --`, and force push.
- Block `gh` for repo operations; the project uses Forgejo/`tea`.
- Block edits on `main` unless a session override tool records explicit user authorization.

Recommended deterministic improvement over Kilo:

- For Rust commands (`cargo`, `bacon`, `rust-analyzer`, `cargo-nextest`, `cargo-deny`), either require the command already runs under `nix develop` or wrap it in `nix develop . --command ...` in a controlled allowlist.
- Add a `toolchain_status` tool that reports branch, dev-shell env, `CARGO_HOME`, `RUSTUP_HOME`, and dirty status.

### Forgejo feedback gate

Register tools:

- `forgejo_inline_reply_payload({ body, path, position })`
- `forgejo_feedback_status({ summary })`
- `forgejo_review_api_recipe({ owner, repo, pr })`

Enforcement:

- Block `gh pr comment` and GitHub-only workflows.
- Block top-level PR comments when inline feedback should be answered in-thread.
- Record unresolved Forgejo feedback in persistent session entries and compaction context.

Recommended deterministic improvement over Kilo:

- Add `forgejo_feedback_checklist` state: every fetched inline comment must be marked `replied`, `fixed`, `deferred`, or `not-applicable` before a top-level summary command is allowed.

### Plan/todo waterfall gate

- If a plan/todo tool or prompt attempts component-waterfall work (`model`, `handler`, `route`, `repository`, `service`, `then add tests`) without naming RED/failing tests/RGR, block or warn.
- Prefer a project command/template that rewrites implementation plans into RGR cycles.

### Read-only reviewer constraints

`pi-subagents` agent files can omit edit/write tools, but project hooks should still be fail-safe:

- If a subagent/session identifies as `*-reviewer`, block `write` and `edit` regardless of prompt text.
- If the extension cannot reliably identify the subagent type from event context, rely on `pi-subagents` tool lists first and keep this as future hardening.

## Agent conversion guidance

For `@tintinweb/pi-subagents`, project agents live in `.pi/agents/<name>.md`. Example read-only reviewer frontmatter:

```markdown
---
description: Read-only reviewer for security, sandboxing, secret handling, unsafe execution, dependencies, and threat-model coupling.
tools: read, grep, find, bash
extensions: true
skills: security-threat-model,rust-workspace-engineering
prompt_mode: append
max_turns: 20
---

You are the security reviewer for `auto_review`...
```

Example edit-capable RGR implementer:

```markdown
---
description: Edit-capable subagent for clearing exactly one current RGR diagnostic with the smallest demanded change.
tools: read, grep, find, bash, edit, write
extensions: true
skills: outside-in-rgr-microcycle,outside-in-tdd,rust-workspace-engineering
prompt_mode: append
max_turns: 30
---

You are the single-diagnostic GREEN implementer...
```

Use `prompt_mode: append` for agents that must inherit `AGENTS.md` and project guardrails. Use `prompt_mode: replace` only for deliberately isolated creative/planning agents.

## Migration phases

### Phase 1: non-destructive Pi parity skeleton

- Add `.pi/settings.json` with pinned packages.
- Add `.mcp.json` for Forgejo MCP via `nix develop`.
- Add `.pi-lens/lsp.json` for Nix Rust LSP.
- Copy skills and command prompts.
- Convert subagent Markdown files.
- Add permission-system project config.

### Phase 2: custom guardrail extension

- Port Kilo plugin behavior to `.pi/extensions/auto-review-guardrails.ts`.
- Add tests for gate behavior if we introduce a JS/TS test harness; otherwise keep implementation minimal and manually verify with Pi tool calls.
- Keep the Kilo files during this phase for rollback/reference.

### Phase 3: deterministic hardening beyond Kilo

- Persist RGR state via Pi session entries.
- Enforce branch-before-edit.
- Bind GREEN to RED command/output.
- Add Forgejo feedback checklist state.
- Wrap/block Rust toolchain commands outside Nix.
- Add PR readiness command that checks: branch, dirty state, RGR complete, feedback resolved, explicit staging only, relevant verification recorded.

### Phase 4: evaluate advanced orchestration

- Revisit `@jwilger/pi-orchestrator` as a first-class deterministic workflow engine after package imports are updated to current `@earendil-works/*` names and the project has a `.orchestra/project.ts` tailored to `auto_review`.
- Consider `pi-agent-flow` only if we want sanitized forked context and flow-state delegation beyond RGR.

## Acceptance checks for migration

Minimum verification before declaring parity:

1. `pi list` shows project packages installed/pinned.
2. `/mcp` shows the Forgejo server; `mcp({ server: "forgejo" })` can list tools without leaking `FORGEJO_TOKEN`.
3. `pi-lens` starts Rust LSP through `nix develop`; no managed/global `rust-analyzer` is downloaded for this repo.
4. Attempting to edit `crates/*/src/*.rs` before `rgr_record_red` is blocked.
5. After `rgr_start` + `rgr_record_red`, the minimal production edit is allowed.
6. `rgr_mark_green` records focused passing output and `rgr_status` survives `/compact` and session resume.
7. `rustup`, `git add .`, `git add -A`, `git commit -a`, `--no-verify`, `git reset --hard`, and force push are blocked.
8. Editing on `main` is blocked unless explicitly overridden.
9. `.env`, `.env.*`, `*.key`, and `*.pem` edits are denied by permission policy.
10. `gh pr comment` and GitHub-only PR workflows are blocked.
11. `forgejo_inline_reply_payload` produces `{ body, path, new_position: position, old_position: 0 }`.
12. Read-only reviewer subagents cannot call edit/write tools.
13. `/tdd-implement`, `/outside-in-rgr`, `/verify`, `/local-review`, `/prepare-forgejo-pr`, and `/process-pr-feedback` are available as Pi prompts or commands.

## Key risks and decisions

- Third-party Pi packages execute code with full local access. Pin versions and review source before installing in project settings.
- `pi-lens` is valuable but must be constrained to Nix for Rust. The `.pi-lens/lsp.json` override is mandatory for this repo.
- `@tintinweb/pi-subagents` and `pi-agent-flow` both use `.pi/agents`; using both initially would be confusing. Start with `pi-subagents` for Kilo parity.
- Kilo plugin code cannot be reused directly because it imports `@kilocode/plugin`; rewrite against Pi's `ExtensionAPI` and event names.
- Do not remove `.kilo/` until the Pi parity acceptance checks pass.
- The most important deterministic improvement is to make RGR/Forgejo/toolchain state persistent and gate-enforced, not merely prompt-instructed.
