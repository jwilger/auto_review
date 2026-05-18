import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { AutoReviewDisciplinePlugin } from "./auto-review-discipline.ts";

test("adr_accept appends supersession notes to partially superseded ADRs", async (t) => {
  const originalCwd = process.cwd();
  const worktree = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-partial-supersede-"));
  t.after(() => {
    process.chdir(originalCwd);
    fs.rmSync(worktree, { recursive: true, force: true });
  });

  fs.mkdirSync(path.join(worktree, "docs"), { recursive: true });
  const supersededPath = path.join(worktree, "docs", "ADR-0001-earlier-decision.md");
  const proposedPath = path.join(worktree, "docs", "ADR-0010-later-decision.md");
  fs.writeFileSync(
    supersededPath,
    `# ADR-0001: Earlier Decision

## Status

Partially superseded

## Superseded By

ADR-0009: Existing partial supersession note

## Context

Earlier context.
`,
  );
  fs.writeFileSync(
    proposedPath,
    `# ADR-0010: Later Decision

## Status

Proposed

## Context

Later context.

## Supersedes

- docs/ADR-0001-earlier-decision.md: Later ADR narrows the remaining decision scope
`,
  );

  process.chdir(worktree);
  const hooks = await AutoReviewDisciplinePlugin({ worktree });

  await assert.doesNotReject(
    hooks.tool.adr_accept.execute({ path: "docs/ADR-0010-later-decision.md" }, { sessionID: "adr-partial-supersede" }),
  );

  assert.match(fs.readFileSync(proposedPath, "utf8"), /## Status\n\nAccepted/);
  assert.match(
    fs.readFileSync(supersededPath, "utf8"),
    /## Status\n\nPartially superseded\n\n## Superseded By\n\nADR-0009: Existing partial supersession note\nADR-0010: Later ADR narrows the remaining decision scope/,
  );
});
