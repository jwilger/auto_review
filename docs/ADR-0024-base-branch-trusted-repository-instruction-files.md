# ADR-0024: Base-Branch Trusted Loading of Repository Instruction Files

## Status

Accepted

## Date

2026-11-16

## Context

Semantic reviews loaded `.auto_review.yaml` from the PR head checkout. The head
is written by the PR author, so a pull request could add `enabled: false` and
receive a success status reading "disabled by repo config", set
`ignored_paths: ["**"]` to hide every file from review, or rewrite `guidelines`
and `override_approvers` to steer its own review. The chat path already read
repository config from the base branch, so the two paths disagreed about which
copy was authoritative.

Comparable review bots (Cursor Bugbot, Anthropic Code Review) read review
configuration only from the base branch: a pull request cannot change how it is
reviewed. Planned features will also ingest more repository-controlled
instruction files, such as `AGENTS.md`, `CLAUDE.md`, `REVIEW.md` and
path-scoped instructions. Each is an attacker-controlled input when read from
the head.

Operators also set review defaults through environment variables such as
`AR_SEVERITY_FLOOR`. As repository config grows keys for the same behaviour,
the project needs a fixed rule for which source wins.

## Decision

Read every repository-controlled instruction file from the PR's base branch
through one trusted loader, and let repository config override operator
defaults.

- `ar_review::trusted_files` is the single runtime loader for repository
  instruction files. `read_trusted_repo_file` fetches a file through
  `ReviewHost::get_file_content` at the base branch, and
  `load_repo_config_from_base` tries `.auto_review.yaml`, then
  `.auto_review.yml`. The result carries provenance (path and ref) and an
  optional warning.
- The loader reads the base branch tip by name, so a config change merged to
  the base applies to open pull requests on their next review.
- The review dispatcher fetches the pull request once, derives a `BaseRef`
  (name and SHA) from it, and passes that into workspace preparation and the
  review. The chat handler delegates to the same loader. The head checkout is
  never a source of repository config.
- The loader fails open to defaults: a missing file, a body over 64 KiB, malformed
  YAML, or a non-404 fetch error yields default config with a warning, and the
  review proceeds. A hostile head cannot turn any of these into a skipped
  review, because the head is not read.
- Future ingestion of `AGENTS.md`, `CLAUDE.md`, `REVIEW.md`, path instructions,
  or any other repository-controlled instruction file must use this loader or
  the base tree, never the head checkout.
- Repository config read from the base branch overrides operator defaults once
  a repository key exists for the same behaviour. Operator environment
  variables are defaults only. No operator variable caps or overrides a
  repository key.

## Consequences

- A pull request cannot disable, narrow, or re-instruct its own review.
  Config changes take effect only after they merge to the base branch.
- Maintainers who want to test a config change must merge it first, or
  validate it locally with `auto-review config validate`.
- Reviews of pull requests that change the config are reviewed under the old
  config. This is intended: the change is reviewed, not applied.
- Each review makes one or two extra host API calls for the config file instead
  of reading the checkout.
- Operators cannot enforce a stricter floor than a repository chooses once that
  repository key exists. Operators who need a hard limit must raise it as a new
  decision rather than reuse an environment default.
- The threat model gains T3a with a dispatcher red-team test for a hostile head
  config.
