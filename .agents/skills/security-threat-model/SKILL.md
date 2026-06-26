---
name: security-threat-model
description: Sandbox, LLM/tool trust boundaries, red-team tests, metrics/docs coupling, and threat-model update triggers.
---

# Security Threat Model

Use this skill when changes touch webhooks, sandboxing, tool execution, LLM inputs or outputs, secrets, dependencies, auth, metrics, or deployment.

## Sources

Read `docs/THREAT-MODEL.md` and the relevant ADRs before changing security-sensitive behavior.

## Review Questions

- Does the normal review path avoid executing untrusted PR code, and do any new execution features introduce a feature-specific sandbox before enabling them?
- Are webhook payloads authenticated before processing?
- Are LLM-issued workspace tools isolated and logged?
- Are secrets kept out of prompts, logs, commits, and review comments?
- Does the change alter documented metrics or alert contracts?

## Coupling

If a documented threat changes, update the matching red-team test when needed. Metrics changes may require updates to Prometheus rules, Grafana dashboards, and contract tests.
