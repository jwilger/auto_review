import assert from "node:assert/strict";
import test from "node:test";

import { AutoReviewDisciplinePlugin } from "../.opencode/plugins/auto-review-discipline.ts";

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
