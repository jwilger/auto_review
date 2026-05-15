# auto_review Pi Guardrails

This Pi setup keeps `AGENTS.md` as the primary project guidance and adds the Kilo-only deltas that must remain explicit:

- Create/switch to a dedicated PR branch before implementation work; do not edit on `main` unless the user explicitly authorizes it.
- Stage commits by explicit path only; never use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
- Use Forgejo (`tea`, Forgejo REST, or Forgejo MCP), not GitHub `gh`, for issue and PR workflows.
- For inline Forgejo review feedback, reply on the inline thread first with `new_position = comment.position` and `old_position = 0`.
- Use the project RGR ledger tools (`rgr_start`, `rgr_record_red`, `rgr_mark_green`, `rgr_mark_refactor`, `rgr_status`) before behavior production Rust edits.
- BDD/TDD discipline is a hard gate: start with one externally visible behavior contract, observe a real RED from an executable command, implement only the single current diagnostic, and stop when the diagnostic changes instead of batching predicted fixes.
- Do not record RED/GREEN from inspection, invented output, or an unavailable command. If a needed command cannot be run, stop immediately, report the blocked state, and propose the missing semantic verification tool.
- Dispatch `.pi/agents/*` through the Pi subagents `Agent` tool for specialist RGR, review, and Forgejo-feedback workflows.
- When a new operation does not fit the available tools' semantics, prefer adding a purpose-built semantic tool or explicit workflow over repurposing existing tools in unintended ways.
