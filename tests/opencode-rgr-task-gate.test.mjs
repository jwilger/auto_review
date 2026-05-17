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
  fs.writeFileSync(path.join(worktree, "README.md"), "initial\n");
  cp.execFileSync("git", ["add", "README.md"], { cwd: worktree });
  cp.execFileSync("git", ["commit", "-m", "initial"], { cwd: worktree, stdio: "ignore" });
  fs.writeFileSync(path.join(worktree, "dirty-change.txt"), "implementation-review veto left this dirty\n");
  return worktree;
}

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
