import importlib
import json
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_module(name):
    return importlib.import_module(f"scripts.codex.{name}")


class CodexPolicyStructureTests(unittest.TestCase):
    def test_repo_has_no_opencode_surface(self):
        tracked = subprocess.check_output(
            ["git", "ls-files"],
            cwd=ROOT,
            text=True,
        ).splitlines()

        leftovers = [
            path
            for path in tracked
            if path == "opencode.json"
            or path.startswith(".opencode/")
            or path.endswith("opencode.json")
        ]

        self.assertEqual([], leftovers)

    def test_codex_project_config_registers_forgejo_mcp_and_hooks(self):
        config = (ROOT / ".codex" / "config.toml").read_text()

        self.assertIn("[mcp_servers.forgejo]", config)
        self.assertIn('command = "sh"', config)
        self.assertIn("git rev-parse --show-toplevel", config)
        self.assertIn("nix run --quiet", config)
        self.assertIn("#forgejo-mcp", config)
        self.assertNotIn("nix develop", config)
        self.assertNotIn("exec forgejo-mcp", config)
        self.assertIn("forgejo-mcp", config)
        self.assertIn("--transport", config)
        self.assertIn("stdio", config)
        self.assertIn("--token", config)
        self.assertIn("FORGEJO_TOKEN", config)
        self.assertIn('env_vars = ["FORGEJO_TOKEN"]', config)
        self.assertIn("[features]", config)
        self.assertIn("hooks = true", config)
        self.assertIn("[agents]", config)
        self.assertIn("max_depth = 1", config)

    def test_codex_hooks_json_runs_pre_tool_policy(self):
        hooks = json.loads((ROOT / ".codex" / "hooks.json").read_text())
        pre_tool = hooks["hooks"]["PreToolUse"]
        commands = [
            hook["command"]
            for group in pre_tool
            for hook in group["hooks"]
        ]

        self.assertTrue(
            any("scripts/codex/pre_tool_use.py" in command for command in commands),
            commands,
        )

    def test_codex_agents_and_skills_replace_opencode_roles(self):
        expected_agents = {
            "architecture-reviewer.toml",
            "docs-operator-reviewer.toml",
            "forgejo-feedback-processor.toml",
            "plan-advisor.toml",
            "rgr-diagnostic-implementer.toml",
            "rgr-implementation-reviewer.toml",
            "rgr-test-author.toml",
            "rgr-test-reviewer.toml",
            "security-reviewer.toml",
            "test-coverage-reviewer.toml",
        }
        actual_agents = {path.name for path in (ROOT / ".codex" / "agents").glob("*.toml")}
        self.assertEqual(expected_agents, actual_agents)

        for skill in [
            "forgejo-feedback-protocol",
            "outside-in-rgr-microcycle",
            "outside-in-tdd",
            "review-taxonomy",
            "rgr-plan-structure",
            "rust-workspace-engineering",
            "security-threat-model",
            "auto-review-codex-workflows",
        ]:
            skill_file = ROOT / ".agents" / "skills" / skill / "SKILL.md"
            self.assertTrue(skill_file.exists(), skill_file)
            text = skill_file.read_text()
            self.assertIn("description:", text)


class CodexRgrTests(unittest.TestCase):
    def test_rgr_state_enforces_one_green_edit_between_diagnostics(self):
        rgr = load_module("rgr")

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            rgr.start(root, "session-1", "visible behavior", "test_name")
            rgr.record_red(root, "session-1", "cargo test test_name", "FAIL: one failing test")
            rgr.approve_red(root, "session-1")

            rgr.assert_edit_allowed(root, "session-1", ["crates/ar-review/src/lib.rs"], branch="feature/test")
            with self.assertRaisesRegex(RuntimeError, "rerun the focused command"):
                rgr.assert_edit_allowed(root, "session-1", ["crates/ar-review/src/lib.rs"], branch="feature/test")

            rgr.record_changed_diagnostic(
                root,
                "session-1",
                "cargo test test_name",
                "FAIL: changed diagnostic",
                "missing method",
            )
            rgr.approve_changed_diagnostic(
                root,
                "session-1",
                "add the missing method only",
                ["crates/ar-review/src/lib.rs"],
            )
            rgr.assert_edit_allowed(root, "session-1", ["crates/ar-review/src/lib.rs"], branch="feature/test")

    def test_rgr_rejects_main_branch_and_unapproved_production_edit(self):
        rgr = load_module("rgr")

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaisesRegex(RuntimeError, "requires RED review approval"):
                rgr.assert_edit_allowed(root, "missing", ["crates/ar-review/src/lib.rs"], branch="feature/test")

            rgr.start(root, "session-1", "visible behavior", "test_name")
            rgr.record_red(root, "session-1", "cargo test test_name", "FAIL: one failing test")
            rgr.approve_red(root, "session-1")
            with self.assertRaisesRegex(RuntimeError, "leaving main"):
                rgr.assert_edit_allowed(root, "session-1", ["crates/ar-review/src/lib.rs"], branch="main")


class CodexAdrTests(unittest.TestCase):
    def test_adr_accept_applies_architecture_patch_and_supersedes_prior_adr(self):
        adr = load_module("adr")

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            docs = root / "docs"
            docs.mkdir()
            (docs / "ARCHITECTURE.md").write_text("old architecture\n")
            prior = docs / "ADR-0001-prior.md"
            prior.write_text("# ADR-0001: Prior\n\n## Status\n\nAccepted\n")

            new_adr = adr.create(
                root,
                title="Codex governance",
                date="2026-06-25",
                context="opencode is retired",
                decision="Codex owns repo-local agent workflow",
                consequences="Codex hooks and helpers are authoritative",
                architecture_patch={
                    "path": "docs/ARCHITECTURE.md",
                    "find": "old architecture",
                    "replace": "new architecture",
                },
                supersedes=[
                    {"path": "docs/ADR-0001-prior.md", "reason": "Codex replaces the prior workflow"}
                ],
            )

            adr.accept(root, new_adr)

            self.assertIn("new architecture", (docs / "ARCHITECTURE.md").read_text())
            self.assertIn("## Status\n\nAccepted", (root / new_adr).read_text())
            self.assertNotIn("## Proposed Architecture Patch", (root / new_adr).read_text())
            self.assertIn("## Status\n\nSuperseded", prior.read_text())
            self.assertIn("## Superseded By", prior.read_text())

    def test_adr_update_reject_and_delete_unmerged_cover_full_workflow(self):
        adr = load_module("adr")

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            docs = root / "docs"
            docs.mkdir()
            (docs / "ARCHITECTURE.md").write_text(
                "current architecture\n- docs/ADR-0010-draft.md\n"
            )
            prior = docs / "ADR-0001-prior.md"
            prior.write_text("# ADR-0001: Prior\n\n## Status\n\nAccepted\n")

            subprocess.run(["git", "init", "-b", "main"], cwd=root, check=True, capture_output=True)
            subprocess.run(["git", "add", "docs/ARCHITECTURE.md", "docs/ADR-0001-prior.md"], cwd=root, check=True)
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-m",
                    "seed docs",
                ],
                cwd=root,
                check=True,
                capture_output=True,
            )

            draft = adr.create(
                root,
                title="Draft",
                date="2026-06-25",
                context="old context",
                decision="old decision",
                consequences="old consequences",
                architecture_patch={
                    "path": "docs/ARCHITECTURE.md",
                    "find": "current architecture",
                    "replace": "updated architecture",
                },
            )

            adr.update(
                root,
                draft,
                title="Draft",
                date="2026-06-26",
                context="new context",
                decision="new decision",
                consequences="new consequences",
                sections_to_update=["date", "decision"],
                architecture_patch={
                    "path": "docs/ARCHITECTURE.md",
                    "find": "current architecture",
                    "replace": "new architecture",
                },
                supersedes=[
                    {"path": "docs/ADR-0001-prior.md", "reason": "Draft narrows the prior decision"}
                ],
            )
            updated = (root / draft).read_text()
            self.assertIn("## Date\n\n2026-06-26", updated)
            self.assertIn("## Context\n\nold context", updated)
            self.assertIn("## Decision\n\nnew decision", updated)
            self.assertIn('"replace": "new architecture"', updated)
            self.assertIn("## Supersedes", updated)

            adr.reject(root, draft, "Rejected in favor of a smaller change")
            rejected = (root / draft).read_text()
            self.assertIn("## Status\n\nRejected", rejected)
            self.assertIn("## Rejection Rationale\n\nRejected in favor of a smaller change", rejected)

            delete_target = docs / "ADR-0010-draft.md"
            delete_target.write_text(
                "# ADR-0010: Draft\n\n## Status\n\nProposed\n\n## Context\n\nDraft.\n"
            )
            referencing = docs / "ADR-0011-reference.md"
            referencing.write_text(
                "# ADR-0011: Reference\n\n## Status\n\nProposed\n\n"
                "mentions docs/ADR-0010-draft.md and ADR-0010-draft.md\n"
            )

            adr.delete_unmerged(root, "docs/ADR-0010-draft.md")

            self.assertFalse(delete_target.exists())
            self.assertNotIn("ADR-0010-draft.md", (docs / "ARCHITECTURE.md").read_text())
            self.assertNotIn("ADR-0010-draft.md", referencing.read_text())


class CodexForgejoAndHookTests(unittest.TestCase):
    def test_forgejo_inline_reply_payload_reuses_original_position(self):
        forgejo = load_module("forgejo")

        payload = forgejo.inline_reply_payload(
            body="@reviewer fixed",
            path="crates/ar-review/src/lib.rs",
            position=27,
        )

        self.assertEqual(
            {
                "body": "@reviewer fixed",
                "path": "crates/ar-review/src/lib.rs",
                "new_position": 27,
                "old_position": 0,
            },
            payload,
        )

    def test_pre_tool_policy_blocks_unsafe_commands(self):
        policy = load_module("policy")

        blocked = [
            "git add .",
            "git add -A",
            "git commit -am 'skip scope'",
            "git reset --hard",
            "git push --force",
            "rustup toolchain install stable",
            "gh pr view 1",
            "tea comment 123 'top level reply'",
            "curl -X POST https://git.johnwilger.com/api/v1/repos/jwilger/auto_review/issues/271/comments -d '{}'",
            "python - <<'PY'\nopen('crates/ar-review/src/lib.rs', 'w').write('x')\nPY",
        ]

        for command in blocked:
            with self.subTest(command=command):
                with self.assertRaises(RuntimeError):
                    policy.check_bash_command(command)

        policy.check_bash_command(
            "curl -X POST https://git.johnwilger.com/api/v1/repos/jwilger/auto_review/pulls/271/reviews/2652/comments -d '{}'"
        )

    def test_pre_tool_policy_blocks_protected_edit_paths(self):
        policy = load_module("policy")

        with self.assertRaisesRegex(RuntimeError, "ADR workflow"):
            policy.check_edit_paths(["docs/ADR-0021-new.md"])

        with self.assertRaisesRegex(RuntimeError, "RGR"):
            policy.check_edit_paths(["crates/ar-review/src/lib.rs"], session_id="missing", branch="feature/x")


class CodexDocsTests(unittest.TestCase):
    def test_agents_md_documents_codex_not_opencode(self):
        text = (ROOT / "AGENTS.md").read_text()

        self.assertIn("Codex uses this file", text)
        self.assertIn("just codex-test", text)
        self.assertNotIn("opencode uses this file", text)
        self.assertNotIn("opencode.json", text)


if __name__ == "__main__":
    unittest.main()
