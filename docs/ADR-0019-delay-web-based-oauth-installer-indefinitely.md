# ADR-0019: Delay Web-Based OAuth Installer Indefinitely

## Status

Accepted

## Date

2026-05-20

## Context

A web-based OAuth installer had been considered as a way to guide operators through Forgejo application setup and initial auto_review configuration. That path would add product surface, authentication flow design, deployment documentation, threat-model obligations, tests, and ongoing maintenance.

Current operator setup is straightforward enough when configured at the organization level in Forgejo. There is no concrete user demand for a browser-based installer, and building it speculatively would distract from the core pull-request review workflow and binary-first deployment path.

## Decision

Delay the web-based OAuth installer indefinitely. Do not keep it on the near-term roadmap, do not design gateway routes or UI flows for it, and do not reserve release or deployment work for it.

Operators should continue configuring the Forgejo organization/application setup directly through existing Forgejo and deployment documentation. The project may revisit a web-based installer only if a real operator need appears with enough detail to justify the added authentication, security, documentation, and support surface.

## Consequences

- The project avoids speculative UI, OAuth, and installer maintenance work.
- Setup remains documentation-led and organization-oriented instead of browser-installer-driven.
- Security review, threat modeling, and tests do not need to cover a web-based installer until the feature is revived by concrete demand.
- Users who prefer a guided browser installer will not have one for now.
- Future proposals for this feature need a fresh ADR or superseding ADR explaining the user need and the additional safety boundaries.

