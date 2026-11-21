# AGENTS.md

`auto_review` is a self-hosted reviewer for Forgejo and GitHub PRs. Use `just`
for routine checks (`just ci`); the [contributor guide](CONTRIBUTING.md) covers
pinned tooling, Rust conventions, required CI, and decision records.

## User constraints

- Follow the existing shipping process: signed commits, green required checks,
  and squash PRs into `main`. The maintainer merges factory changes (including
  `.pi/`) and controls releases. See [factory policy](factory/policy.md).
- Test behavior changes first. A user-visible change needs a regression through
  the affected public interface, with a focused failure for the intended reason
  before implementation.
- Repository admins decide whether review blocks merges. Pi may send private
  repository content to already-configured model providers, but not credentials.
  Ask before changing anything outside this project or granting access to an
  account, credential, or service.
- Review third-party tooling before installing or running it: code and install
  scripts, provenance, maintenance, data access and transfer, and permissions
  (including remote writes/deletes). Pin exact revisions when supported; review
  update diffs and record the review alongside the change. Skip unverified tools.

## Preferences, not gates

- Aim for real findings with at least the precision and accuracy of a staff
  engineer familiar with the stack. The maintainer reports company-wide CI use
  and useful findings over successive rounds of AI-authored PRs; the benchmark
  and measurement method are open.
- Target required CI in 10 minutes with warm caches. Prefer a pinned Pi version
  and a static check for Pi-only changes. Keep CLI output and PR comments
  understandable without color or emoji.
- Prefer primary work by Scott Wlaschin, Kent Beck, Adam Dymitruk, and Martin
  Dilger where relevant. Keep architecture decisions in the existing ADR stream;
  a package README and its PR suffice for non-architectural harness experiments.
  No decision yet on when to split out a service.

## Pi harness

- Save skills, extensions, packages, and MCP configs in the project. When one
  needs loading, call `reload_and_continue` as the sole final tool call; name the
  resource and next step, and verify it loaded before use. See the
  [package README](.pi/packages/reload-and-continue/README.md) for first use.
- Improve the harness only in response to observed friction. Look for an
  existing solution first; cite primary sources for consequential decisions and
  keep evidence and open questions in the repo. Record the problem, rationale,
  verification, and when to revisit. Prefer small checks with passing and failing
  cases over guidance for objective rules. Retest and retire what stops paying off.
- Check the available models before choosing; compare quality, cost, latency,
  and data handling rather than assuming a roster.
