# ar-tools

Legacy linter runner crate retained while runtime linter execution is retired.
Normal review/orchestrator jobs no longer call this crate; deterministic
linters/tests/builds belong in CI before semantic review is triggered.

## Public surface

| Module | Content |
|--------|---------|
| `runner::LinterRunner` | The trait every runner implements: `name()` + `run(workspace, sandbox) -> Vec<Finding>`. |
| `runner::run_in_sandbox` | Helper that wraps a `SandboxCommand` so production runs go through `ar-sandbox`. |
| `finding::{Finding, Severity}` | Normalised result type. Severity maps 1:1 to `ar_prompts::ReviewSeverity`. |
| `catalog::{LinterInfo, linter_catalogue}` | Static `&[LinterInfo]` enumerating the legacy linter catalogue (name, description, language tags, homepage). |
| Per-tool modules | `actionlint`, `ansible_lint`, `ast_grep`, `bandit`, `biome`, `buf`, `checkov`, `cppcheck`, `dotenv_linter`, `eslint`, `gitleaks`, `golangci_lint`, `gosec`, `hadolint`, `helm`, `htmlhint`, `jsonlint`, `ktlint`, `kubeconform`, `markdownlint`, `mypy`, `nilaway`, `osv_scanner`, `oxlint`, `phpstan`, `pmd`, `prettier`, `pylint`, `rubocop`, `ruff`, `semgrep`, `shellcheck`, `shfmt`, `sqlfluff`, `staticcheck`, `stylelint`, `swiftlint`, `taplo`, `tflint`, `trivy`, `typos`, `vale`, `vint`, `yamllint`. |

## Tests

`cargo test -p ar-tools` covers every parser against captured tool
output (so a tool version change can be detected without rerunning
the actual binary). The `catalog.rs` self-checks pin alphabetical
ordering, no duplicate names, the bundled-count, and that every
entry has a non-empty description + https homepage.

## Dependencies

`serde_json` for tools that emit JSON, no other heavy deps. The
sandbox boundary lives in `ar-sandbox`.
