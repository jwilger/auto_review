import assert from "node:assert/strict";
import cp from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { AutoReviewDisciplinePlugin } from "../plugins/auto-review-discipline.ts";

function createDirtyWorktree() {
  const worktree = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-rgr-veto-"));
  cp.execFileSync("git", ["init"], { cwd: worktree, stdio: "ignore" });
  cp.execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: worktree });
  cp.execFileSync("git", ["config", "user.name", "Test User"], { cwd: worktree });
  cp.execFileSync("git", ["config", "commit.gpgsign", "false"], { cwd: worktree });
  cp.execFileSync("git", ["config", "core.hooksPath", "/dev/null"], { cwd: worktree });
  fs.writeFileSync(path.join(worktree, "README.md"), "initial\n");
  cp.execFileSync("git", ["add", "README.md"], { cwd: worktree });
  cp.execFileSync("git", ["commit", "-m", "initial"], { cwd: worktree, stdio: "ignore" });
  fs.writeFileSync(path.join(worktree, "dirty-change.txt"), "implementation-review veto left this dirty\n");
  return worktree;
}

function createCleanMainWorktree() {
  const worktree = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-main-branch-gate-"));
  cp.execFileSync("git", ["init", "--initial-branch=main"], { cwd: worktree, stdio: "ignore" });
  cp.execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: worktree });
  cp.execFileSync("git", ["config", "user.name", "Test User"], { cwd: worktree });
  cp.execFileSync("git", ["config", "commit.gpgsign", "false"], { cwd: worktree });
  cp.execFileSync("git", ["config", "core.hooksPath", "/dev/null"], { cwd: worktree });
  fs.mkdirSync(path.join(worktree, "crates", "demo", "src"), { recursive: true });
  fs.writeFileSync(path.join(worktree, "crates", "demo", "src", "lib.rs"), "pub fn demo() {}\n");
  cp.execFileSync("git", ["add", "crates/demo/src/lib.rs"], { cwd: worktree });
  cp.execFileSync("git", ["commit", "-m", "initial"], { cwd: worktree, stdio: "ignore" });
  return worktree;
}

function claimTokenlessLeaseFromFreshPluginProcess(worktree: string, sessionID: string) {
  const claimScript = `
import { AutoReviewDisciplinePlugin } from "./.opencode/plugins/auto-review-discipline.ts";

const worktree = process.env.RGR_WORKTREE;
if (!worktree) {
  throw new Error("RGR_TEST: missing RGR_WORKTREE for claim script");
}

const hooks = await AutoReviewDisciplinePlugin({ worktree: worktree });
await hooks.tool.rgr_claim_implementation_lease.execute({}, { sessionID: process.env.RGR_SESSION_ID ?? "rgr-fresh-delegated-session" });
console.log("RGR_TEST_TOKENLESS_CLAIM_OK");
`;

  return cp.spawnSync("node", ["--input-type=module", "-e", claimScript], {
    encoding: "utf8",
    cwd: process.cwd(),
    env: {
      ...process.env,
      RGR_WORKTREE: worktree,
      RGR_SESSION_ID: sessionID,
    },
  });
}

function delegateTokenlessImplementationLeaseFromFreshParentProcess(
  worktree: string,
  sessionID: string,
  focusedCommand: string,
  delegatePrompt: string,
) {
  const delegationScript = `
import { AutoReviewDisciplinePlugin } from "./.opencode/plugins/auto-review-discipline.ts";

const worktree = process.env.RGR_WORKTREE;
const sessionID = process.env.RGR_SESSION_ID;
const focusedCommand = process.env.RGR_FOCUSED_COMMAND;
const delegatePrompt = process.env.RGR_DELEGATE_PROMPT;

if (!worktree) {
  throw new Error("RGR_TEST: missing RGR_WORKTREE for parent delegation script");
}
if (!sessionID) {
  throw new Error("RGR_TEST: missing RGR_SESSION_ID for parent delegation script");
}
if (!focusedCommand) {
  throw new Error("RGR_TEST: missing RGR_FOCUSED_COMMAND for parent delegation script");
}
if (!delegatePrompt) {
  throw new Error("RGR_TEST: missing RGR_DELEGATE_PROMPT for parent delegation script");
}

const hooks = await AutoReviewDisciplinePlugin({ worktree });
await hooks.tool.rgr_start.execute(
  {
    behavior:
      "delegated implementation lease should be reissued across parent process boundaries when claimed tokenlessly",
    test:
      "supports tokenless delegated implementation lease after a changed diagnostic on restartless parent session",
  },
  { sessionID },
);

await hooks.tool.rgr_record_red.execute(
  {
    command: focusedCommand,
    output: "one focused plugin test failed",
  },
  { sessionID },
);
await hooks.tool.rgr_approve_red.execute({}, { sessionID });
await hooks.tool.rgr_record_changed_diagnostic.execute(
  {
    command: focusedCommand,
    output: "FAIL: follow-up changed diagnostic after tokenless delegated lease claim",
    diagnostic: "follow-up changed diagnostic requires a second scoped implementation edit",
  },
  { sessionID },
);
await hooks.tool.rgr_approve_changed_diagnostic.execute(
  {
    allowedImmediateChange:
      "Allow a second scoped implementation edit in crates/demo/src/lib.rs for the follow-up changed diagnostic.",
    allowedPaths: ["crates/demo/src/lib.rs"],
  },
  { sessionID },
);

await hooks["tool.execute.before"](
  { tool: "task", sessionID },
  {
    args: {
      subagent_type: "rgr-diagnostic-implementer",
      prompt: delegatePrompt,
    },
  },
);

console.log("RGR_TEST_TOKENLESS_DELEGATION_OK");
`;

  return cp.spawnSync("node", ["--input-type=module", "-e", delegationScript], {
    encoding: "utf8",
    cwd: process.cwd(),
    env: {
      ...process.env,
      RGR_WORKTREE: worktree,
      RGR_SESSION_ID: sessionID,
      RGR_FOCUSED_COMMAND: focusedCommand,
      RGR_DELEGATE_PROMPT: delegatePrompt,
    },
  });
}

test("blocks production Rust edit tool changes on main after RED approval", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-on-main-with-red-approval";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "main branch production Rust edit gate",
      test: "blocks production Rust edit tool changes on main after RED approval",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'blocks production Rust edit tool changes on main after RED approval'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "edit", sessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
    /Branch gate: production Rust edits under crates\/\*\/src require leaving main or calling the explicit override tool\./,
  );
});

test("blocks shell bypass editing production Rust file during active approved RED", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-shell-bypass"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-shell-bypass-attempt";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "deterministic shell command bypasses must be blocked during approved RED",
      test: "blocks shell bypass editing production Rust file during active approved RED",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'blocks shell bypass editing production Rust file during active approved RED'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "bash", sessionID },
      {
        args: {
          command:
            "python - <<'PY'\nfrom pathlib import Path\nPath('crates/demo/src/lib.rs').write_text('inline bypass check')\nPY",
        },
      },
    ),
    /RGR.*shell command.*bypass/i,
  );
});

test("distinguishes read-only open from cat write redirection in shell bypass guard", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-shell-bypass-granularity"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-shell-bypass-granularity-check";

  await hooks.tool.rgr_start.execute(
    {
      behavior:
        "shell bypass guard must allow read-only open(...) while still blocking write-producing shell redirection",
      test: "distinguishes read-only open from cat write redirection in shell bypass guard",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'distinguishes read-only open from cat write redirection in shell bypass guard'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  let openRejected = false;
  let catRejected = false;
  let openError: unknown;
  let catError: unknown;

  try {
    await hooks["tool.execute.before"](
      { tool: "bash", sessionID },
      {
        args: {
          command:
            "python - <<'PY'\nfrom pathlib import Path\nwith open('crates/demo/src/lib.rs', 'r', encoding='utf-8') as f:\n    data = f.read()\n    print(data)\nPY",
        },
      },
    );
  } catch (error) {
    openRejected = true;
    openError = error;
  }

  try {
    await hooks["tool.execute.before"](
      { tool: "bash", sessionID },
      {
        args: {
          command:
            "cat > crates/demo/src/lib.rs <<'EOF'\n# shell write bypass check\npub fn rgr_guard_demo() {}\nEOF",
        },
      },
    );
  } catch (error) {
    catRejected = true;
    catError = error;
  }

  assert.equal(openRejected, false, `Read-only open('crates/demo/src/lib.rs') should not be treated as a write bypass. Guard error: ${String(openError?.toString?.() ?? openError)}`);
  assert.equal(catRejected, true, "cat > production Rust path should be rejected as a shell write bypass.");
  assert.match(String(catError), /RGR shell command bypass/i, "cat redirection should fail with an RGR shell bypass error.");
});

test("blocks rgr-test-author task delegation when no RGR cycle is active", async () => {
  const hooks = await AutoReviewDisciplinePlugin({ worktree: process.cwd() });
  const output = {
    args: {
      subagent_type: "rgr-test-author",
      prompt: "Write the next RED test for an unstarted behavior.",
    },
  };

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "task", sessionID: "session-without-rgr-cycle" },
      output,
    ),
    /RGR task gate: start an RGR cycle with rgr_start before delegating to rgr-test-author; recover by starting the cycle or asking the orchestrator to do so\./,
  );
});

test("allows other task delegations that mention rgr-test-author", async () => {
  const hooks = await AutoReviewDisciplinePlugin({ worktree: process.cwd() });
  const output = {
    args: {
      subagent_type: "explore",
      prompt: "Find existing guidance that mentions rgr-test-author.",
    },
  };

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "task", sessionID: "session-without-rgr-cycle-for-explore" },
      output,
    ),
  );
});

test("rgr-test-author can claim a delegated Rust in-source test edit lease before RED approval", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-test-author-lease"], { stdio: "ignore" });
  fs.writeFileSync(
    path.join(worktree, "crates", "demo", "src", "lib.rs"),
    "pub fn demo() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn existing_unit_test() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n",
  );
  cp.execFileSync("git", ["-C", worktree, "add", "crates/demo/src/lib.rs"]);
  cp.execFileSync("git", ["-C", worktree, "commit", "-m", "add unit test module"], { stdio: "ignore" });
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-rust-in-source-red-test-author";
  const subagentSessionID = "session-rust-in-source-red-test-author-subagent";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "Rust unit RED tests may live inside source files",
      test: "rgr-test-author delegation may request Rust in-source unit test edits before RED approval",
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "task", sessionID },
      {
        args: {
          subagent_type: "rgr-test-author",
          sessionID: subagentSessionID,
          prompt:
            "Write only the next RED test. The test may be an adjacent Rust unit test inside #[cfg(test)] mod tests in crates/ar-review/src/pipeline.rs. Do not edit production code.",
        },
      },
    ),
  );

  await hooks.tool.rgr_claim_test_author_lease.execute({}, { sessionID: subagentSessionID });

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "edit", sessionID: subagentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
  );
});

test("delegated rgr-test-author lease rejects Rust source edits without in-source tests", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-test-author-lease-scope"], { stdio: "ignore" });
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-test-author-scope-parent";
  const subagentSessionID = "session-with-test-author-scope-subagent";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "rgr-test-author lease is scoped to in-source Rust test modules",
      test: "delegated rgr-test-author lease rejects Rust source edits without in-source tests",
    },
    { sessionID },
  );
  await hooks["tool.execute.before"](
    { tool: "task", sessionID },
    {
      args: {
        subagent_type: "rgr-test-author",
        sessionID: subagentSessionID,
        prompt: "Write only the next RED test in a Rust #[cfg(test)] module. Do not edit production code.",
      },
    },
  );
  await hooks.tool.rgr_claim_test_author_lease.execute({}, { sessionID: subagentSessionID });

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "edit", sessionID: subagentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
    /RGR gate: delegated rgr-test-author Rust source edits require an in-source #\[cfg\(test\)\]\s*mod tests in the target file\./,
  );
});

test("delegated rgr-test-author lease rejects multi-file Rust source edits", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-test-author-lease-one-file"], { stdio: "ignore" });
  fs.writeFileSync(
    path.join(worktree, "crates", "demo", "src", "lib.rs"),
    "pub fn demo() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn existing_unit_test() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n",
  );
  fs.writeFileSync(
    path.join(worktree, "crates", "demo", "src", "other.rs"),
    "pub fn other() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn existing_unit_test() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n",
  );
  cp.execFileSync("git", ["-C", worktree, "add", "crates/demo/src/lib.rs", "crates/demo/src/other.rs"]);
  cp.execFileSync("git", ["-C", worktree, "commit", "-m", "add unit test modules"], { stdio: "ignore" });
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-test-author-one-file-parent";
  const subagentSessionID = "session-with-test-author-one-file-subagent";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "rgr-test-author lease is limited to one Rust source file",
      test: "delegated rgr-test-author lease rejects multi-file Rust source edits",
    },
    { sessionID },
  );
  await hooks["tool.execute.before"](
    { tool: "task", sessionID },
    {
      args: {
        subagent_type: "rgr-test-author",
        sessionID: subagentSessionID,
        prompt: "Write only one next RED test in a Rust #[cfg(test)] module. Do not edit production code.",
      },
    },
  );
  await hooks.tool.rgr_claim_test_author_lease.execute({}, { sessionID: subagentSessionID });

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "apply_patch", sessionID: subagentSessionID },
      {
        args: {
          patchText:
            "*** Begin Patch\n*** Update File: crates/demo/src/lib.rs\n@@\n pub fn demo() {}\n*** Update File: crates/demo/src/other.rs\n@@\n pub fn other() {}\n*** End Patch",
        },
      },
    ),
    /RGR gate: delegated rgr-test-author Rust source edits are limited to one target file\./,
  );
});

test("prevents parent edit after delegated implementation lease is consumed", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-lease-parent-lock"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-session-with-lease";
  const subagentSessionID = "rgr-implementation-subagent-session-with-lease";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "parent must not consume delegated implementation lease",
      test: "prevents parent edit after delegated implementation lease is consumed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'prevents parent edit after delegated implementation lease is consumed'",
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  const delegation = {
    args: {
      subagent_type: "rgr-diagnostic-implementer",
      sessionID: subagentSessionID,
      prompt:
        "Current diagnostic: delegated implementation lease was granted to subagent. Allowed immediate change: one production Rust edit inside lease scope.",
    },
  };

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "task", sessionID: parentSessionID }, delegation),
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID: subagentSessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );

  await assert.rejects(
    hooks["tool.execute.before"]({ tool: "edit", sessionID: parentSessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
    /RGR gate: another behavioral production edit requires rerunning the focused command and recording RED or GREEN first\./,
  );
});

test("allows rgr-test-author after recording implementation-review veto recovery on a dirty worktree", async (t) => {
  const worktree = createDirtyWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-implementation-review-veto-recovery";

  await hooks.tool.rgr_recover_implementation_review_veto.execute(
    {
      veto: "implementation-reviewer vetoed GREEN because the dirty worktree lacks recovery context",
      nextRedScope: "one focused RED for issue #233 recovery delegation",
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "task", sessionID },
      {
        args: {
          subagent_type: "rgr-test-author",
          prompt: "Write the next RED test for issue #233 recovery delegation.",
        },
      },
    ),
  );
});

test("consumes implementation-review veto recovery after one rgr-test-author delegation", async (t) => {
  const worktree = createDirtyWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-consumed-implementation-review-veto-recovery";
  const delegation = {
    args: {
      subagent_type: "rgr-test-author",
      prompt: "Write the next RED test for issue #233 recovery delegation.",
    },
  };

  await hooks.tool.rgr_recover_implementation_review_veto.execute(
    {
      veto: "implementation-reviewer vetoed GREEN because recovery must delegate one protected RED",
      nextRedScope: "one focused RED for issue #233 recovery delegation",
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "task", sessionID }, delegation),
  );

  await assert.rejects(
    hooks["tool.execute.before"]({ tool: "task", sessionID }, delegation),
    /RGR task gate: start an RGR cycle with rgr_start before delegating to rgr-test-author; recover by starting the cycle or asking the orchestrator to do so\./,
  );
});

test("grants one delegated implementation edit after parent-approved RED", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-subagent-lease"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-session";
  const subagentSessionID = "rgr-implementation-subagent-session";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "orchestrator-approved RED powers one edit in delegated diagnostic session",
      test: "grants one delegated implementation edit after parent-approved RED",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'grants one delegated implementation edit after parent-approved RED'",
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  const delegation = {
    args: {
      subagent_type: "rgr-diagnostic-implementer",
      sessionID: subagentSessionID,
      prompt:
        "Current diagnostic: parent-approved RED is not visible to subagent edit hook. Allowed immediate change: grant a scoped one-edit implementation lease to the delegated subagent session.",
    },
  };

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "task", sessionID: parentSessionID }, delegation),
  );
  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID: subagentSessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );
  await assert.rejects(
    hooks["tool.execute.before"]({ tool: "edit", sessionID: subagentSessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
    /RGR gate: another behavioral production edit requires rerunning the focused command and recording RED or GREEN first\./,
  );
});

test("delegates changed-diagnostic implementation edit to a scoped subagent session", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-delegated-changed-diagnostic-lease"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-changed-diagnostic-lease";
  const subagentSessionID = "rgr-implementation-subagent-changed-diagnostic-lease";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'delegates changed-diagnostic implementation edit to a scoped subagent session'";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "delegated changed-diagnostic lease can be consumed by diagnostic implementer",
      test: "delegates changed-diagnostic implementation edit to a scoped subagent session",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID: parentSessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );

  fs.writeFileSync(path.join(worktree, "crates/demo/src", "other.rs"), "pub fn demo_other() {}\n");

  await hooks.tool.rgr_record_changed_diagnostic.execute(
    {
      command: focusedCommand,
      output: "FAIL: changed diagnostic after first production edit",
      diagnostic: "expected provider metadata, got undefined",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange:
        "Apply a scoped production change to the provider dispatch path that addresses the changed diagnostic without widening edit scope.",
      allowedPaths: ["crates/demo/src/lib.rs", "crates/demo/src/other.rs"],
    },
    { sessionID: parentSessionID },
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          sessionID: subagentSessionID,
          prompt:
            "Current diagnostic: expected provider metadata, got undefined. Allowed immediate change: update the provider dispatch path only in scoped production files.",
        },
      },
    ),
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "apply_patch", sessionID: subagentSessionID },
      {
        args: {
          patchText:
            "*** Begin Patch\n*** Update File: crates/demo/src/lib.rs\n@@\n*** End Patch\n*** Begin Patch\n*** Update File: crates/demo/src/other.rs\n@@\n*** End Patch\n",
        },
      },
    ),
  );
});

test("issues claimable lease for changed-diagnostic delegation without explicit subagent session", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-claimable-diagnostic-lease"], { stdio: "ignore" });
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-missing-subagent-session-id";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'issues claimable lease for changed-diagnostic delegation without explicit subagent session'";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "missing subagent session IDs should still produce a claimable implementation lease",
      test: "issues claimable lease for changed-diagnostic delegation without explicit subagent session",
    },
    { sessionID: parentSessionID },
  );

  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });
  await hooks.tool.rgr_record_changed_diagnostic.execute(
    {
      command: focusedCommand,
      output: "FAIL: changed diagnostic after first production edit",
      diagnostic: "expected provider metadata, got undefined",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange:
        "Update the provider metadata dispatch path only in scoped production Rust files.",
      allowedPaths: ["crates/demo/src/lib.rs", "crates/demo/src/other.rs"],
    },
    { sessionID: parentSessionID },
  );

  const delegation = await hooks["tool.execute.before"](
    { tool: "task", sessionID: parentSessionID },
    {
      args: {
        subagent_type: "rgr-diagnostic-implementer",
        prompt:
          "Current diagnostic: expected provider metadata, got undefined. Allowed immediate change: scoped provider metadata dispatch fix.",
      },
    },
  );

  const claimTool = (hooks.tool as Record<string, { execute?: unknown }>).rgr_claim_implementation_lease;
  assert.equal(Object.prototype.hasOwnProperty.call(delegation ?? {}, "claimToken"), false);
  assert.equal(
    typeof claimTool?.execute,
    "function",
    "Expected a claim API for delegated diagnostic leases",
  );

  await assert.doesNotReject(
    (claimTool.execute as (args: {}, context: { sessionID: string }) => Promise<unknown>)(
      {},
      { sessionID: "rgr-implementation-claiming-subagent" },
    ),
  );
});

test("does not expose or accept claimToken for delegated implementation lease claims", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-tokenless-claim-api"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-tokenless-claim-api";
  const focusedCommand =
    "node --test --test-name-pattern 'does not expose or accept claimToken for delegated implementation lease claims' .opencode/plugins/auto-review-discipline-rgr.test.ts";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "delegation contract should be tokenless",
      test: "does not expose or accept claimToken for delegated implementation lease claims",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  const delegation = await hooks["tool.execute.before"](
    { tool: "task", sessionID: parentSessionID },
    {
      args: {
        subagent_type: "rgr-diagnostic-implementer",
        prompt:
          "Current diagnostic: tokenless claim API contract should not return claim tokens. Allowed immediate change: one scoped production edit in crates/demo/src/lib.rs.",
      },
    },
  );

  assert.equal(
    Object.prototype.hasOwnProperty.call(delegation ?? {}, "claimToken"),
    false,
    "tool.execute.before for delegated diagnostic tasks should not expose claimToken anymore",
  );

  const claimTool = hooks.tool.rgr_claim_implementation_lease;
  await assert.rejects(
    (claimTool.execute as (args: { claimToken?: string }, context: { sessionID: string }) => Promise<unknown>)(
      { claimToken: "legacy-token" },
      { sessionID: "rgr-tokened-implementation-claim" },
    ),
    /RGR gate: delegated implementation leases are claimed from central state; claimToken arguments are not accepted\./,
  );

  await assert.doesNotReject(
    claimTool.execute({}, { sessionID: "rgr-tokenless-implementation-claim" }),
  );
});

test("supports tokenless delegated implementation lease across fresh plugin instances", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));

  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-cross-instance-tokenless-claim"], { stdio: "ignore" });

  const parentSessionID = "rgr-parent-cross-instance-tokenless-claim";
  const subagentSessionID = "rgr-subagent-cross-instance-tokenless-claim";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'supports tokenless delegated implementation lease across fresh plugin instances'";

  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "delegation state can outlive plugin instance boundaries",
      test: "supports tokenless delegated implementation lease across fresh plugin instances",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "edit", sessionID: parentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
  );

  await parentHooks.tool.rgr_record_changed_diagnostic.execute(
    {
      command: focusedCommand,
      output: "FAIL: changed diagnostic after first production edit",
      diagnostic: "current diagnostic should require an implementation scope update",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange:
        "Allow a scoped implementation edit in crates/demo/src/lib.rs for the updated changed diagnostic.",
      allowedPaths: ["crates/demo/src/lib.rs"],
    },
    { sessionID: parentSessionID },
  );

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: parent-approved changed diagnostic requires implementation. Allowed immediate change: scoped crates/demo/src/lib.rs patch.",
        },
      },
    ),
  );

  const subagentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await assert.doesNotReject(
    // Tokenless claim is the intended contract; this should succeed when state is persisted and shared.
    subagentHooks.tool.rgr_claim_implementation_lease.execute({}, { sessionID: subagentSessionID }),
  );

  await assert.doesNotReject(
    subagentHooks["tool.execute.before"](
      { tool: "edit", sessionID: subagentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
  );

  await parentHooks.tool.rgr_record_changed_diagnostic.execute(
    {
      command: focusedCommand,
      output: "FAIL: changed diagnostic after first tokenless delegated production edit",
      diagnostic: "follow-up changed diagnostic requires a second scoped implementation edit",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange:
        "Allow a second scoped implementation edit in crates/demo/src/lib.rs for the follow-up changed diagnostic.",
      allowedPaths: ["crates/demo/src/lib.rs"],
    },
    { sessionID: parentSessionID },
  );

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: follow-up changed diagnostic requires a second scoped implementation edit in crates/demo/src/lib.rs. Allowed immediate change: update the follow-up production scope in crates/demo/src/lib.rs only.",
        },
      },
    ),
  );

  await assert.doesNotReject(
    // Tokenless claim should continue to work for a second token from the same session.
    subagentHooks.tool.rgr_claim_implementation_lease.execute({}, { sessionID: subagentSessionID }),
  );

  await assert.doesNotReject(
    subagentHooks["tool.execute.before"](
      { tool: "edit", sessionID: subagentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
  );
});

test("supports tokenless delegated implementation lease after a changed diagnostic on restartless parent session", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-tokenless-after-changed-diagnostic"], { stdio: "ignore" });
  fs.writeFileSync(path.join(worktree, ".gitignore"), ".opencode/state/\n");
  cp.execFileSync("git", ["-C", worktree, "add", ".gitignore"], { stdio: "ignore" });
  cp.execFileSync("git", ["-C", worktree, "commit", "-m", "Ignore RGR state directory for tokenless lease tests"], { stdio: "ignore" });

  const parentSessionID = "rgr-parent-tokenless-after-changed-diagnostic";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'supports tokenless delegated implementation lease after a changed diagnostic on restartless parent session'";
  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior:
        "a fresh-process claimed delegated lease should not poison later same-session tokenless lease issuance",
      test: "supports tokenless delegated implementation lease after a changed diagnostic on restartless parent session",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: first delegated implementation lease. Allowed immediate change: one scoped production edit in crates/demo/src/lib.rs.",
        },
      },
    ),
  );

  const firstDelegatedClaim = claimTokenlessLeaseFromFreshPluginProcess(worktree, "rgr-fresh-subagent-tokenless-claim-1");
  assert.equal(
    firstDelegatedClaim.status,
    0,
    `Fresh plugin process should claim the first delegated lease. stdout=${firstDelegatedClaim.stdout}; stderr=${firstDelegatedClaim.stderr}`,
  );
  assert.match(String(firstDelegatedClaim.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);

  const delegationFromRestartedParent = delegateTokenlessImplementationLeaseFromFreshParentProcess(
    worktree,
    parentSessionID,
    focusedCommand,
    "Current diagnostic: follow-up changed diagnostic requires a second scoped implementation edit. Allowed immediate change: one scoped production edit in crates/demo/src/lib.rs.",
  );

  assert.equal(
    delegationFromRestartedParent.status,
    0,
    `Fresh parent process should reissue a tokenless diagnostic lease after the changed diagnostic. stdout=${delegationFromRestartedParent.stdout}; stderr=${delegationFromRestartedParent.stderr}`,
  );
  assert.match(String(delegationFromRestartedParent.stdout), /RGR_TEST_TOKENLESS_DELEGATION_OK/);

  const secondDelegatedClaim = claimTokenlessLeaseFromFreshPluginProcess(worktree, "rgr-fresh-subagent-tokenless-claim-2");
  assert.equal(
    secondDelegatedClaim.status,
    0,
    `Fresh plugin process should claim the second delegated lease after a changed diagnostic. stdout=${secondDelegatedClaim.stdout}; stderr=${secondDelegatedClaim.stderr}`,
  );
  assert.match(String(secondDelegatedClaim.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);
});

test("allows a new RGR cycle on the same parent session after a tokenless claim", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));

  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-same-session-tokenless-cycle-reuse"], {
    stdio: "ignore",
  });
  fs.writeFileSync(path.join(worktree, ".gitignore"), ".opencode/state/\n");
  cp.execFileSync("git", ["-C", worktree, "add", ".gitignore"], { stdio: "ignore" });
  cp.execFileSync("git", ["-C", worktree, "commit", "-m", "Ignore RGR state directory for tokenless lease reuse"], {
    stdio: "ignore",
  });

  const sessionID = "rgr-parent-same-session-tokenless-cycle-reuse";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'allows a new RGR cycle on the same parent session after a tokenless claim'";
  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "stale delegated lease claims should not poison future same-session cycles",
      test: "allows a new RGR cycle on the same parent session after a tokenless claim",
    },
    { sessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: initial delegated lease cycle. Allowed immediate change: one scoped production edit in crates/demo/src/lib.rs.",
        },
      },
    ),
  );

  const staleTokenlessClaim = claimTokenlessLeaseFromFreshPluginProcess(
    worktree,
    "rgr-fresh-subagent-tokenless-claim-prior-cycle",
  );
  assert.equal(
    staleTokenlessClaim.status,
    0,
    `Fresh process should claim the delegated lease from prior cycle. stdout=${staleTokenlessClaim.stdout}; stderr=${staleTokenlessClaim.stderr}`,
  );
  assert.match(String(staleTokenlessClaim.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "fresh same-session cycle should ignore stale tokenless lease rows",
      test: "allows a new RGR cycle on the same parent session after a tokenless claim",
    },
    { sessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "edit", sessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
  );
});

test("reloads tokenless delegated implementation lease from persisted worktree state after plugin restart", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));

  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-process-boundary-tokenless-claim"], { stdio: "ignore" });

  const parentSessionID = "rgr-parent-cross-instance-tokenless-claim-process-boundary";
  const subagentSessionID = "rgr-subagent-cross-instance-tokenless-claim-process-boundary";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'reloads tokenless delegated implementation lease from persisted worktree state after plugin restart'";

  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "delegated leases should survive plugin process restarts",
      test: "reloads tokenless delegated implementation lease from persisted worktree state after plugin restart",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    parentHooks.tool.rgr_record_changed_diagnostic.execute(
      {
        command: focusedCommand,
        output: "FAIL: process-boundary changed diagnostic after red approved",
        diagnostic: "delegated lease claim must be portable across plugin restarts",
      },
      { sessionID: parentSessionID },
    ),
  );
  await parentHooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange:
        "Implement the changed diagnostic fix in crates/demo/src/lib.rs using a scoped delegated implementation edit.",
      allowedPaths: ["crates/demo/src/lib.rs"],
    },
    { sessionID: parentSessionID },
  );

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: delegated lease portability must include restart boundaries. Allowed immediate change: scoped delegated implementation edit in crates/demo/src/lib.rs.",
        },
      },
    ),
  );

  const restartResult = claimTokenlessLeaseFromFreshPluginProcess(worktree, subagentSessionID);

  assert.equal(
    restartResult.status,
    0,
    `Fresh plugin process should claim the delegated lease after a process-boundary restart. stdout=${restartResult.stdout}; stderr=${restartResult.stderr}`,
  );
  assert.match(String(restartResult.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);
});

test("rejects parent production Rust edits after tokenless fresh-process claim", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-parent-edit-blocked-after-tokenless-claim"], {
    stdio: "ignore",
  });

  const parentSessionID = "rgr-parent-session-tokenless-claimed-by-fresh-process";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'rejects parent production Rust edits after tokenless fresh-process claim'";
  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "fresh-process tokenless delegate should consume parent edit allowance",
      test: "rejects parent production Rust edits after tokenless fresh-process claim",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: parent RED should delegate implementation edit; allowed immediate change: one scoped production edit in crates/demo/src/lib.rs.",
        },
      },
    ),
  );

  const freshProcessClaim = claimTokenlessLeaseFromFreshPluginProcess(worktree, "rgr-fresh-implementation-session");
  assert.equal(
    freshProcessClaim.status,
    0,
    `Fresh process should successfully claim the delegated lease. stdout=${freshProcessClaim.stdout}; stderr=${freshProcessClaim.stderr}`,
  );
  assert.match(String(freshProcessClaim.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);

  await assert.rejects(
    parentHooks["tool.execute.before"](
      { tool: "edit", sessionID: parentSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
    /RGR gate: another behavioral production edit requires rerunning the focused command and recording RED or GREEN first\./,
  );
});

test("requires sqlite-backed, gitignored RGR state for tokenless delegated leases", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));

  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-sqlite-state-lease"], { stdio: "ignore" });

  const parentSessionID = "rgr-parent-sqlite-state-lease";
  const subagentSessionID = "rgr-subagent-sqlite-state-lease";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'requires sqlite-backed, gitignored RGR state for tokenless delegated leases'";

  const parentHooks = await AutoReviewDisciplinePlugin({ worktree });

  await parentHooks.tool.rgr_start.execute(
    {
      behavior: "delegated lease persistence should require a sqlite-backed, gitignored state",
      test: "requires sqlite-backed, gitignored RGR state for tokenless delegated leases",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await parentHooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  await assert.doesNotReject(
    parentHooks["tool.execute.before"](
      { tool: "task", sessionID: parentSessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt:
            "Current diagnostic: delegated lease persistence should be sqlite-backed and tokenless. Allowed immediate change: verify one scoped production edit via the delegated lease path.",
        },
      },
    ),
  );

  const statePath = path.join(worktree, ".opencode", "state", "rgr.sqlite");
  assert.equal(
    fs.existsSync(statePath),
    true,
    `Expected tokenless lease persistence to create ${statePath}.`,
  );

  const ignoreCheck = cp.spawnSync("git", ["-C", worktree, "check-ignore", "-v", ".opencode/state/rgr.sqlite"], {
    encoding: "utf8",
  });
  assert.equal(
    ignoreCheck.status,
    0,
    `Expected .opencode/state/rgr.sqlite to be ignored by gitignore. git check-ignore output: ${ignoreCheck.stdout || ignoreCheck.stderr}`,
  );

  const freshClaimResult = claimTokenlessLeaseFromFreshPluginProcess(worktree, subagentSessionID);

  assert.equal(
    freshClaimResult.status,
    0,
    `Fresh plugin process should tokenlessly claim the delegated lease. stdout=${freshClaimResult.stdout}; stderr=${freshClaimResult.stderr}`,
  );
  assert.match(String(freshClaimResult.stdout), /RGR_TEST_TOKENLESS_CLAIM_OK/);
});

test("permits a second production edit after changed RED verification is re-recorded for the same command", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-red-refresh"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-changed-red-output";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'permits a second production edit after changed RED verification is re-recorded for the same command'";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "changed RED output for same focused command grants one refreshed edit token",
      test: "permits a second production edit after recording a changed RED output for the same command",
    },
    { sessionID },
  );

  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed: initial diagnostic",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );

  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "FAIL: one focused plugin test failed: changed diagnostic after first production edit",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );
});

test("changed diagnostic tools refresh edit allowance without starting a new RED", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-changed-diagnostic-tools"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-changed-diagnostic-tools";
  const focusedCommand =
    "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'changed diagnostic tools refresh edit allowance without starting a new RED'";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "changed diagnostic tools refresh the GREEN edit allowance",
      test: "changed diagnostic tools refresh edit allowance without starting a new RED",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: focusedCommand,
      output: "one focused plugin test failed: initial diagnostic",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );

  await hooks.tool.rgr_record_changed_diagnostic.execute(
    {
      command: focusedCommand,
      output: "FAIL: one focused plugin test failed: changed diagnostic after first production edit",
      diagnostic: "expected base URL, got empty string",
    },
    { sessionID },
  );

  await assert.rejects(
    hooks["tool.execute.before"]({ tool: "edit", sessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
    /RGR gate: changed diagnostic requires approval before the next production edit\./,
  );

  await hooks.tool.rgr_approve_changed_diagnostic.execute(
    {
      allowedImmediateChange: "Expose provider metadata and use it in the router usage callback.",
      allowedPaths: ["crates/demo/src/lib.rs", "crates/demo/src/other.rs"],
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"](
      { tool: "apply_patch", sessionID },
      {
        args: {
          patchText:
            "*** Begin Patch\n*** Update File: crates/demo/src/lib.rs\n@@\n*** End Patch\n*** Begin Patch\n*** Update File: crates/demo/src/other.rs\n@@\n*** End Patch\n",
        },
      },
    ),
  );
});

test("does not consume implementationEditToken when a multi-file apply_patch is rejected", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-multifile-apply-patch"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-rgr-multifile-apply-patch-rejects";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "multi-file apply_patch rejection should not consume the one production edit token",
      test: "does not consume implementationEditToken when a multi-file apply_patch is rejected",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'does not consume implementationEditToken when a multi-file apply_patch is rejected'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  fs.writeFileSync(path.join(worktree, "crates/demo/src", "other.rs"), "pub fn demo_other() {}\n");

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "apply_patch", sessionID },
      {
        args: {
          patchText:
            "*** Begin Patch\n*** Update File: crates/demo/src/lib.rs\n@@\n*** End Patch\n*** Begin Patch\n*** Update File: crates/demo/src/other.rs\n@@\n*** End Patch\n",
        },
      },
    ),
    /RGR gate: production Rust edit paths are outside the approved diagnostic scope\./,
  );

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "edit", sessionID }, { args: { filePath: "crates/demo/src/lib.rs" } }),
  );
});

test("does not create consumable unscoped diagnostic lease without subagent session", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  cp.execFileSync("git", ["-C", worktree, "checkout", "-b", "feature/rgr-unscoped-diagnostic-lease"], { stdio: "ignore" });

  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const parentSessionID = "rgr-parent-session-with-missing-subagent-id";
  const unrelatedSessionID = "rgr-unrelated-production-session";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "delegation must include subagent session id",
      test: "does not create consumable unscoped diagnostic lease without subagent session",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'does not create consumable unscoped diagnostic lease without subagent session'",
      output: "one focused plugin test failed",
    },
    { sessionID: parentSessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID: parentSessionID });

  const delegation = {
    args: {
      subagent_type: "rgr-diagnostic-implementer",
      prompt:
        "Current diagnostic: approved parent RED is being delegated. Allowed immediate change: one scoped production edit lease.",
    },
  };

  await assert.doesNotReject(
    hooks["tool.execute.before"]({ tool: "task", sessionID: parentSessionID }, delegation),
  );

  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "edit", sessionID: unrelatedSessionID },
      { args: { filePath: "crates/demo/src/lib.rs" } },
    ),
    /RGR gate: production Rust edits under crates\/\*\/src require RED review approval recorded with rgr_approve_red\./,
  );
});

test("blocks GREEN after edit-capable Task completion until proof-of-work verification is recorded", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-pending-proof-of-work-after-task";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "edit-capable Task completion requires proof-of-work verification before GREEN",
      test: "blocks GREEN after edit-capable Task completion until proof-of-work verification is recorded",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'blocks GREEN after edit-capable Task completion until proof-of-work verification is recorded'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  if (typeof hooks["tool.execute.after"] === "function") {
    await hooks["tool.execute.after"](
      { tool: "task", sessionID },
      {
        args: {
          subagent_type: "rgr-diagnostic-implementer",
          prompt: "Current diagnostic: focused test fails. Allowed immediate change: smallest production edit that changes this diagnostic.",
        },
        result: "Edited production code and returned control to the orchestrator.",
      },
    );
  }

  await assert.rejects(
    hooks.tool.rgr_mark_green.execute(
      { output: "rgr-diagnostic-implementer says the implementation is done" },
      { sessionID },
    ),
    /RGR gate: record explicit proof-of-work verification before marking GREEN\./,
  );
});

test("rejects vague GREEN verification output without concrete command evidence", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-vague-green-verification-output";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "GREEN verification output must include a concrete command and pass/fail evidence",
      test: "rejects vague GREEN verification output without concrete command evidence",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: "node --test --test-name-pattern 'rejects vague GREEN verification output without concrete command evidence' .opencode/plugins/auto-review-discipline-rgr.test.ts",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });

  await assert.rejects(
    hooks.tool.rgr_mark_green.execute({ output: "tests pass" }, { sessionID }),
    /RGR gate: GREEN verification output must include a concrete command and pass\/fail evidence\./,
  );
});

test("recording proof-of-work verification clears pending proof before GREEN", async (t) => {
  const worktree = createCleanMainWorktree();
  t.after(() => fs.rmSync(worktree, { recursive: true, force: true }));
  const hooks = await AutoReviewDisciplinePlugin({ worktree });
  const sessionID = "session-with-verified-proof-of-work-after-task";

  await hooks.tool.rgr_start.execute(
    {
      behavior: "explicit proof-of-work verification clears pending proof before GREEN",
      test: "recording proof-of-work verification clears pending proof before GREEN",
    },
    { sessionID },
  );
  await hooks.tool.rgr_record_red.execute(
    {
      command: "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'",
      output: "one focused plugin test failed",
    },
    { sessionID },
  );
  await hooks.tool.rgr_approve_red.execute({}, { sessionID });
  await hooks["tool.execute.after"](
    { tool: "task", sessionID },
    {
      args: {
        subagent_type: "rgr-diagnostic-implementer",
        prompt: "Current diagnostic: focused test fails. Allowed immediate change: smallest production edit that changes this diagnostic.",
      },
      result: "Edited production code and returned control to the orchestrator.",
    },
  );

  await hooks.tool.rgr_record_proof_of_work_verification.execute(
    {
      output:
        "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'\nPASS: one focused plugin test passed after the implementation edit",
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks.tool.rgr_mark_green.execute(
      {
        output:
          "node --test .opencode/plugins/auto-review-discipline-rgr.test.ts --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'\nPASS: explicit proof-of-work verification cleared pending proof before GREEN",
      },
      { sessionID },
    ),
  );
});
