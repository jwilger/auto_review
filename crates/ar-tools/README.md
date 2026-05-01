# ar-tools

44 bundled linter runners. Each one wraps a binary (e.g. `ruff`,
`shellcheck`, `gitleaks`) so the orchestrator can collect
structured `Finding`s without each call site re-implementing
exec + parse.

## Public surface

| Module | Content |
|--------|---------|
| `runner::LinterRunner` | The trait every runner implements: `name()` + `run(workspace, sandbox) -> Vec<Finding>`. |
| `runner::run_in_sandbox` | Helper that wraps a `SandboxCommand` so production runs go through `ar-sandbox`. |
| `finding::{Finding, Severity}` | Normalised result type. Severity maps 1:1 to `ar_prompts::ReviewSeverity`. |
| `catalog::{LinterInfo, linter_catalogue}` | Static `&[LinterInfo]` enumerating every bundled linter (name, description, language tags, homepage). Backs `auto_review list-linters`. A contract test in `ar_review::routing` enforces the catalog covers every routed runner. |
| Per-tool modules | `actionlint`, `ansible_lint`, `ast_grep`, `bandit`, `biome`, `buf`, `checkov`, `cppcheck`, `dotenv_linter`, `eslint`, `gitleaks`, `golangci_lint`, `gosec`, `hadolint`, `helm`, `htmlhint`, `jsonlint`, `ktlint`, `kubeconform`, `markdownlint`, `mypy`, `nilaway`, `osv_scanner`, `oxlint`, `phpstan`, `pmd`, `prettier`, `pylint`, `rubocop`, `ruff`, `semgrep`, `shellcheck`, `shfmt`, `sqlfluff`, `staticcheck`, `stylelint`, `swiftlint`, `taplo`, `tflint`, `trivy`, `typos`, `vale`, `vint`, `yamllint`. |

## Adding a new linter

See `CONTRIBUTING.md` for the canonical checklist. In short:

1. Pick the binary's JSON or text output format.
2. Implement `parse_<tool>_output(...) -> Result<Vec<Finding>, RunnerError>` as a pure function (test against captured tool output).
3. Build a `<Tool>Runner` that calls `run_in_sandbox` and feeds the
   stdout into the parser. Never spawn `tokio::process::Command`
   directly.
4. Wire routing in `crates/ar-review/src/routing.rs::select_runners`.
5. Add an entry to `catalog.rs` and update its `assert_eq!` count.
6. Bundle the binary in `deploy/Dockerfile.sandbox`.

## Tests

`cargo test -p ar-tools` covers every parser against captured tool
output (so a tool version change can be detected without rerunning
the actual binary). The `catalog.rs` self-checks pin alphabetical
ordering, no duplicate names, the bundled-count, and that every
entry has a non-empty description + https homepage.

## Dependencies

`serde_json` for tools that emit JSON, no other heavy deps. The
sandbox boundary lives in `ar-sandbox`.
