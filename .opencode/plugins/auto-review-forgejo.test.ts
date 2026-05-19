import assert from "node:assert/strict";
import test from "node:test";

import { AutoReviewForgejoPlugin } from "./auto-review-forgejo.ts";

const buildInput = (sessionID: string) => ({
  tool: "bash",
  sessionID,
});

test("forges PR creation on issue branch requires closure trailer", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-pr-no-trailer-${Date.now()}`;

  await assert.rejects(
    () =>
      hooks["tool.execute.before"]?.(buildInput(sessionID), {
        args: {
          command:
            "tea pr create --repo jwilger/auto_review --head issue-220-clarify-release-scope --base main --title 'Release fix' --description 'WIP: updates'",
        },
      }),
    /Include one closure trailer/,
  );
});

test("forgejo PR creation passes with closes trailer in description", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-pr-closes-${Date.now()}`;

  await assert.doesNotReject(() =>
    hooks["tool.execute.before"]?.(buildInput(sessionID), {
      args: {
        command:
          "tea pr create --repo jwilger/auto_review --head issue-220-clarify-release-scope --base main --title 'Release fix' --description 'Closes #220: close original issue\nAdds safer metadata checks'",
      },
    }),
  );
});

test("forgejo PR creation rejects when description lacks closure trailer", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-pr-missing-body-trailer-${Date.now()}`;

  await assert.rejects(
    () =>
      hooks["tool.execute.before"]?.(buildInput(sessionID), {
        args: {
          command:
            "forgejo pr create --repo jwilger/auto_review --head issue-221-bug-fix --base main --title 'Fixes #221: close issue' --description 'Adds extra guard rails'",
        },
      }),
    /Include one closure trailer in the PR description/,
  );
});

test("non issue-linked branches can skip trailer requirement", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-pr-non-issue-${Date.now()}`;

  await assert.doesNotReject(() =>
    hooks["tool.execute.before"]?.(buildInput(sessionID), {
      args: {
        command:
          "tea pr create --repo jwilger/auto_review --head feature-cool-improvement --base main --title 'Update docs' --description 'Refactors docs and examples'",
      },
    }),
  );
});

test("blocks top-level Forgejo issue comments as inline feedback replies", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-top-level-comment-${Date.now()}`;

  await assert.rejects(
    () =>
      hooks["tool.execute.before"]?.(buildInput(sessionID), {
        args: {
          command:
            "curl -X POST https://git.johnwilger.com/api/v1/repos/jwilger/auto_review/issues/271/comments -d '{\"body\":\"Addressed\"}'",
        },
      }),
    /inline feedback replies must use the existing review comment thread/,
  );
});

test("allows existing Forgejo review-thread comment replies", async () => {
  const hooks = await AutoReviewForgejoPlugin();
  const sessionID = `forgejo-inline-reply-${Date.now()}`;

  await assert.doesNotReject(() =>
    hooks["tool.execute.before"]?.(buildInput(sessionID), {
      args: {
        command:
          "curl -X POST https://git.johnwilger.com/api/v1/repos/jwilger/auto_review/pulls/271/reviews/2652/comments -d '{\"body\":\"@auto-review Addressed\",\"path\":\"flake.nix\",\"new_position\":69,\"old_position\":0}'",
      },
    }),
  );
});
