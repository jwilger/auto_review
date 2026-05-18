import assert from "node:assert/strict";
import cp from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { AutoReviewDisciplinePlugin } from "../.opencode/plugins/auto-review-discipline.ts";

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
      command: "node --test tests/opencode-rgr-task-gate.test.mjs --test-name-pattern 'blocks production Rust edit tool changes on main after RED approval'",
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
      command: "node --test tests/opencode-rgr-task-gate.test.mjs --test-name-pattern 'blocks GREEN after edit-capable Task completion until proof-of-work verification is recorded'",
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
      command: "node --test --test-name-pattern 'rejects vague GREEN verification output without concrete command evidence' tests/opencode-rgr-task-gate.test.mjs",
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
      command: "node --test tests/opencode-rgr-task-gate.test.mjs --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'",
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
        "node --test tests/opencode-rgr-task-gate.test.mjs --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'\nPASS: one focused plugin test passed after the implementation edit",
    },
    { sessionID },
  );

  await assert.doesNotReject(
    hooks.tool.rgr_mark_green.execute(
      {
        output:
          "node --test tests/opencode-rgr-task-gate.test.mjs --test-name-pattern 'recording proof-of-work verification clears pending proof before GREEN'\nPASS: explicit proof-of-work verification cleared pending proof before GREEN",
      },
      { sessionID },
    ),
  );
});
