# auto_review Pi Guardrails

This Pi setup keeps `AGENTS.md` as the primary project guidance and adds the Kilo-only deltas that must remain explicit:

- Create/switch to a dedicated PR branch before implementation work; do not edit on `main` unless the user explicitly authorizes it.
- Stage commits by explicit path only; never use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
- When pi-lens is enabled, work with its deferred `agent_end` formatting/autofix: after `write`/`edit`, do not commit, push, or open a PR until a follow-up turn has rechecked `toolchain_status`, reviewed the post-hook diff, and rerun the relevant verification.
- Use Forgejo (`tea`, Forgejo REST, or Forgejo MCP), not GitHub `gh`, for issue and PR workflows.
- For inline Forgejo review feedback, reply on the inline thread first with `new_position = comment.position` and `old_position = 0`.
- Use the project RGR ledger tools (`rgr_start`, `rgr_record_red`, `rgr_mark_green`, `rgr_mark_refactor`, `rgr_status`) before behavior production Rust edits.
- Dispatch `.pi/agents/*` through the Pi subagents `Agent` tool for specialist RGR, review, and Forgejo-feedback workflows.
