# Security policy

`auto_review` is a Forgejo PR-review bot that deliberately accepts
attacker-controlled input (every PR), clones PR workspaces for semantic
context, calls LLM providers, and holds a Forgejo PAT with write access
to its watched repos. Deterministic linters/tests/builds run in CI, not
inside the gateway review runtime. The threat surface is real. We take
disclosures seriously and want vulnerability researchers to feel
welcome.

## Reporting a vulnerability

**Do not file a public issue.** A public issue describing an
exploit gives attackers a head start before patches are available.

Email **john@johnwilger.com** with:

- A clear subject line starting with `[auto_review security]`.
- A short description of the vulnerability and its impact.
- Reproduction steps. A failing test case or proof-of-concept
  patch is ideal but not required.
- Whether you want public credit when the fix ships, and the
  name / handle / link to use.

Encrypted email is welcome. Ask for a PGP key if you want one and
we'll publish it here.

If you don't get an acknowledgement within five business days,
please re-send — assume mail got lost rather than ignored.

## Disclosure timeline

Pre-1.0 we don't make rigid commitments, but the working norms are:

| Phase | Target |
|-------|--------|
| Acknowledge receipt | Within 5 business days |
| First-pass triage (severity / scope) | Within 14 days |
| Fix landed on `main` for confirmed issues | Within 90 days |
| Coordinated disclosure | After the fix ships, or at the 90-day mark, whichever comes first |

If a fix needs longer than 90 days, we'll explain why and agree on
an extended timeline with the reporter rather than disclose
unilaterally.

## Scope

In scope:
- Anything in this repository's `crates/` source tree.
- The shipped deploy artefacts under `deploy/` (Dockerfile,
  Helm chart, systemd unit, Forgejo Action template).
- Default configurations and example env files.
- The bundled prompts and JSON schemas under
  `crates/ar-prompts/`.

Out of scope (these are upstream / operator concerns):
- Forgejo, Gitea, or any other Git forge the bot talks to.
- Specific LLM providers (OpenAI, Anthropic, Ollama, vLLM).
- CI-owned linters/tests/build tools — report those to their respective
  upstreams or to the operator's CI environment owner.
- Operator-controlled configuration: an operator who sets
  `WEBHOOK_SECRET=hunter2` is responsible for that decision.

## What's already documented

Read [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md) before reporting
— it enumerates known attacker profiles, trust boundaries, and the
mitigations in place. A report that's a re-statement of an
already-documented threat (with no new exploit) gets a thanks-but
response. New attack vectors against documented threats are
exactly what we want to hear about.

## Hall of fame

Researchers who report valid vulnerabilities get listed here when
the fix ships, unless they ask not to be:

_(empty so far — be the first.)_

## License of disclosed reports

By submitting a vulnerability report you grant us permission to
fix it, publish a description after the fix ships, and credit you
(or not, per your preference). You retain copyright on your
report; we don't claim ownership.
