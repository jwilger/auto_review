import assert from "node:assert/strict";
import test from "node:test";

import { AutoReviewContextPlugin } from "./auto-review-context.ts";
import { AutoReviewDisciplinePlugin } from "./auto-review-discipline.ts";
import { clearCycle, setCycle } from "./lib/shared.ts";

test("context plugin owns compaction context when discipline plugin is also loaded", async () => {
  const sessionID = `context-owner-${Date.now()}`;
  setCycle(sessionID, {
    behavior: "avoid duplicated compaction context",
    test: "context plugin owns compaction context",
    stage: "red",
  });

  try {
    const contextPlugin = await AutoReviewContextPlugin({});
    const disciplinePlugin = await AutoReviewDisciplinePlugin({ worktree: process.cwd() });
    const input = { sessionID };
    const output: { context: string[] } = { context: [] };

    await contextPlugin["experimental.session.compacting"]?.(input, output);
    await disciplinePlugin["experimental.session.compacting"]?.(input, output);

    assert.deepEqual(output.context, [
      "auto_review project context:",
      'Active RGR cycle: {"behavior":"avoid duplicated compaction context","test":"context plugin owns compaction context","stage":"red"}',
    ]);
  } finally {
    clearCycle(sessionID);
  }
});
