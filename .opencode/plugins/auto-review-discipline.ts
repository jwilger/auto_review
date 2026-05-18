import { tool, type Plugin } from "@opencode-ai/plugin";
import cp from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { assertCleanWorktree, getCycle, consumeDelegatedImplementationEditLease, consumeImplementationReviewVetoRecovery, isNonBehavioralPath, isProductionRustPath, isLikelyTestPath, recordDelegatedImplementationEditLease, recordImplementationReviewVetoRecovery, recordTouchedFile, setCycle, clearCycle, recordVerification, sessionContext, validateRgrRedEvidence } from "./lib/shared.ts";

function filePathFromArgs(args: unknown): string | undefined {
  if (!args || typeof args !== "object") return undefined;
  const record = args as Record<string, unknown>;
  const path = record.filePath ?? record.file_path ?? record.path;
  return typeof path === "string" ? path : undefined;
}

function changedPathsFromArgs(args: unknown): string[] {
  if (!args || typeof args !== "object") return [];
  const record = args as Record<string, unknown>;
  const directPath = record.filePath ?? record.file_path ?? record.path;
  if (typeof directPath === "string") return [directPath];
  const patchText = record.patchText;
  if (typeof patchText !== "string") return [];
  return Array.from(patchText.matchAll(/^\*\*\* Update File: (.+)$/gm), (match) => match[1]);
}

function isEditTool(toolID: string): boolean {
  return /(^|\.)(edit|write|apply_patch)$/i.test(toolID) || /apply_patch/i.test(toolID);
}

function isProtectedAdrPath(filePath) {
  const normalized = (path.isAbsolute(filePath) ? path.relative(process.cwd(), filePath) : filePath).replaceAll("\\", "/");
  return /^docs\/ADR-[^/]*\.md$/i.test(normalized) || normalized === "docs/ARCHITECTURE.md";
}

function patchTouchesProtectedAdrPath(args) {
  if (!args || typeof args !== "object") return false;
  const patchText = (args as Record<string, unknown>).patchText;
  if (typeof patchText !== "string") return false;
  for (const match of patchText.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)) {
    if (isProtectedAdrPath(match[1].trim())) return true;
  }
  for (const match of patchText.matchAll(/^\*\*\* Move to: (.+)$/gm)) {
    if (isProtectedAdrPath(match[1].trim())) return true;
  }
  return false;
}

function rejectsWaterfallTodo(args: unknown): boolean {
  const text = JSON.stringify(args ?? "").toLowerCase();
  const componentWords = ["model", "handler", "route", "repository", "service", "then add tests"];
  const hasComponents = componentWords.filter((word) => text.includes(word)).length >= 2;
  return hasComponents && !text.includes("red") && !text.includes("failing test") && !text.includes("rgr");
}

function requireString(record, field) {
  const value = record[field];
  if (typeof value !== "string" || value.trim() === "") throw new Error(`adr_create requires ${field}`);
  return value;
}

function titleSlug(title) {
  return title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function nextAdrNumber(docsDir) {
  let highest = 0;
  for (const entry of fs.readdirSync(docsDir, { withFileTypes: true })) {
    if (!entry.isFile()) continue;
    const match = /^ADR-(\d+)-.*\.md$/i.exec(entry.name);
    if (match) highest = Math.max(highest, Number.parseInt(match[1], 10));
  }
  return highest + 1;
}

function applyArchitecturePatch(patch) {
  if (!patch || typeof patch !== "object") throw new Error("adr_create requires architecturePatch");
  const patchRecord = patch;
  const patchPath = requireString(patchRecord, "path");
  const find = requireString(patchRecord, "find");
  const replace = requireString(patchRecord, "replace");
  if (patchPath.replaceAll("\\", "/") !== "docs/ARCHITECTURE.md") {
    throw new Error("adr_create architecturePatch.path must be docs/ARCHITECTURE.md");
  }
  const current = fs.readFileSync(patchPath, "utf8");
  if (!current.includes(find)) throw new Error("adr_create architecturePatch.find was not found");
  fs.writeFileSync(patchPath, current.replace(find, replace));
}

function proposedArchitecturePatchSection(patch) {
  if (!patch || typeof patch !== "object") throw new Error("adr_create requires architecturePatch");
  const patchRecord = patch;
  const storedPatch = {
    path: requireString(patchRecord, "path"),
    find: requireString(patchRecord, "find"),
    replace: requireString(patchRecord, "replace"),
  };
  return `## Proposed Architecture Patch\n\nPatch:\n${JSON.stringify(storedPatch)}\n`;
}

function replaceProposedArchitecturePatchSection(text, patch) {
  const section = proposedArchitecturePatchSection(patch).trimEnd();
  if (!/^## Proposed Architecture Patch\n\n/m.test(text)) return `${text.trimEnd()}\n\n${section}\n`;
  return text.replace(/(^|\n)## Proposed Architecture Patch\n\n[\s\S]*?(?=\n## |\s*$)/, `$1${section}`);
}

function storedProposedArchitecturePatch(text) {
  const jsonMatch = /^## Proposed Architecture Patch\n\nPatch:\n([^\n]+)\n/m.exec(text);
  if (jsonMatch) return JSON.parse(jsonMatch[1]);
  const match = /^## Proposed Architecture Patch\n\nPath: ([^\n]+)\nFind:\n([\s\S]*?)\nReplace:\n([\s\S]*?)(?=\n## |\s*$)/m.exec(text);
  if (!match) return undefined;
  return { path: match[1].trim(), find: match[2].trim(), replace: match[3].trim() };
}

function removeProposedArchitecturePatchSection(text) {
  return text.replace(/\n?## Proposed Architecture Patch\n\n[\s\S]*?(?=\n## |\s*$)/, "");
}

function recordedSupersedes(text) {
  const supersedes = adrSectionValue(text, "Supersedes");
  if (!supersedes) return [];
  return supersedes.split("\n").map((line) => /^- ([^:]+): (.+)$/.exec(line)).filter(Boolean).map((match) => ({ path: match[1], reason: match[2] }));
}

function adrSectionValue(text, section) {
  const match = new RegExp(`^## ${section}\\n\\n([\\s\\S]*?)(?=\\n## |$)`, "m").exec(text);
  return match?.[1]?.trim();
}

function replaceAdrSection(text, section, value) {
  return text.replace(new RegExp(`(^## ${section}\\n\\n)([\\s\\S]*?)(?=\\n## |$)`, "m"), `$1${value.trim()}\n`);
}

function appendSupersededBy(text, supersedingAdrPath, reason) {
  const note = `${adrIdFromPath(supersedingAdrPath)}: ${reason}`;
  const status = adrSectionValue(text, "Status");
  let updated = status === "Accepted" ? replaceAdrSection(text, "Status", "Superseded") : text;
  const existing = adrSectionValue(updated, "Superseded By");
  if (existing) return replaceAdrSection(updated, "Superseded By", `${existing}\n${note}`);
  return replaceAdrSection(updated, "Status", `${adrSectionValue(updated, "Status")}\n\n## Superseded By\n\n${note}`);
}

function adrIdFromPath(adrPath) {
  return /^ADR-(\d+)/.exec(path.basename(adrPath))?.[0] ?? path.basename(adrPath, ".md");
}

function requireAcceptedSupersedes(supersedes) {
  for (const entry of supersedes) {
    const supersededPath = requireString(entry, "path");
    requireString(entry, "reason");
    if (!isAdrDocumentPath(supersededPath) || !fs.existsSync(supersededPath)) {
      throw new Error("supersedes entries must reference existing ADR files");
    }
    const superseded = fs.readFileSync(supersededPath, "utf8");
    if (!["Accepted", "Superseded", "Partially superseded"].includes(adrSectionValue(superseded, "Status"))) {
      throw new Error("supersedes entries must reference Accepted ADRs");
    }
  }
}

function gitPathExistsOnMain(filePath) {
  const gitPath = filePath.replaceAll("\\", "/");
  const main = cp.spawnSync("git", ["rev-parse", "--verify", "main^{commit}"], { stdio: "ignore" });
  if (main.status !== 0) throw new Error("adr_delete_unmerged cannot verify main; refusing to delete ADR");
  const result = cp.spawnSync("git", ["ls-tree", "--name-only", "main", "--", gitPath], { encoding: "utf8" });
  if (result.status !== 0) throw new Error("adr_delete_unmerged cannot check whether ADR exists on main");
  return result.stdout.trim() !== "";
}

function isAdrDocumentPath(filePath) {
  return /^docs\/ADR-[^/]*\.md$/i.test(filePath.replaceAll("\\", "/"));
}

function removeLinesReferencing(text, references) {
  return text
    .split("\n")
    .filter((line) => !references.some((reference) => line.includes(reference)))
    .join("\n");
}

function cleanAdrReferences(deletedAdrPath, context) {
  const references = [deletedAdrPath.replaceAll("\\", "/"), path.basename(deletedAdrPath)];
  const architecturePath = "docs/ARCHITECTURE.md";
  if (fs.existsSync(architecturePath)) {
    const current = fs.readFileSync(architecturePath, "utf8");
    const updated = removeLinesReferencing(current, references);
    if (updated !== current) {
      fs.writeFileSync(architecturePath, updated);
      recordTouchedFile(context.sessionID, architecturePath);
    }
  }
  for (const entry of fs.readdirSync("docs", { withFileTypes: true })) {
    if (!entry.isFile() || !/^ADR-.*\.md$/i.test(entry.name)) continue;
    const adrPath = path.join("docs", entry.name);
    const current = fs.readFileSync(adrPath, "utf8");
    const updated = removeLinesReferencing(current, references);
    if (updated !== current) {
      fs.writeFileSync(adrPath, updated);
      recordTouchedFile(context.sessionID, adrPath);
    }
  }
}

function rejectsBroadDiagnosticTask(args: unknown): boolean {
  const text = JSON.stringify(args ?? "").toLowerCase();
  if (!text.includes("rgr-diagnostic-implementer")) return false;
  const namesDiagnostic = /current\s+diagnostic|diagnostic/.test(text);
  const namesAllowedChange = text.includes("allowed immediate change") || text.includes("allowed change");
  const broadFix = text.includes("fix all") || text.includes("all failures") || text.includes("fix everything");
  return broadFix || !namesDiagnostic || !namesAllowedChange;
}

function isRgrTestAuthorTask(args: unknown): boolean {
  if (!args || typeof args !== "object") return false;
  return (args as Record<string, unknown>).subagent_type === "rgr-test-author";
}

function isRgrDiagnosticImplementerTask(args: unknown): boolean {
  if (!args || typeof args !== "object") return false;
  return (args as Record<string, unknown>).subagent_type === "rgr-diagnostic-implementer";
}

function delegatedSubagentSessionFromTaskArgs(args: unknown): string | undefined {
  if (!args || typeof args !== "object") return undefined;
  const record = args as Record<string, unknown>;
  const candidateKeys = [
    "sessionID",
    "sessionId",
    "session_id",
    "subagent_session_id",
    "subagentSessionId",
    "subagentSessionID",
    "subagent_session",
    "subagentSession",
    "task_session_id",
    "taskSessionId",
  ];
  for (const key of candidateKeys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value;
  }
  return undefined;
}

function isOnMainBranch(worktree: string | undefined): boolean {
  const result = cp.spawnSync("git", ["-C", worktree ?? process.cwd(), "branch", "--show-current"], { encoding: "utf8" });
  return result.status === 0 && result.stdout.trim() === "main";
}

function validateRgrGreenVerificationOutput(output: string): void {
  const hasConcreteCommand = /\b(?:cargo|just|node|npm|pnpm|yarn|bacon|nix)\b[^\n]*(?:test|nextest|fmt|clippy|build|ci)/i.test(output);
  const hasPassFailEvidence = /\b(?:pass(?:ed|es)?|fail(?:ed|ure)?|ok|success)\b/i.test(output);
  if (!hasConcreteCommand || !hasPassFailEvidence) {
    throw new Error("RGR gate: GREEN verification output must include a concrete command and pass/fail evidence.");
  }
}

export const AutoReviewDisciplinePlugin: Plugin = async ({ worktree } = {}) => ({
  tool: {
    rgr_start: tool({
      description: "Start an auto_review RED-GREEN-REFACTOR cycle for one behavior.",
      args: {
        behavior: tool.schema.string().describe("Observable behavior under test"),
        test: tool.schema.string().describe("Specific failing test name or path"),
      },
      async execute(args, context) {
        assertCleanWorktree(worktree);
        setCycle(context.sessionID, { behavior: args.behavior, test: args.test, stage: "red" });
        return `RGR cycle started for ${args.behavior}. Record observed RED output before production edits.`;
      },
    }),
    rgr_record_red: tool({
      description: "Record observed failing test output for the active RGR cycle.",
      args: {
        command: tool.schema.string().describe("Focused test command that failed"),
        output: tool.schema.string().min(1).describe("Copied failing output from the actual run"),
      },
      async execute(args, context) {
        const current = getCycle(context.sessionID);
        if (!current) throw new Error("Start an RGR cycle before recording RED.");
        validateRgrRedEvidence(args.output);
        setCycle(context.sessionID, { ...current, command: args.command, failingOutput: args.output, reviewedRed: false, implementationEditToken: false, stage: "red" });
        return "RED recorded. RED review approval is required before production edits.";
      },
    }),
    rgr_approve_red: tool({
      description: "Approve the recorded RED evidence before production edits.",
      args: {},
      async execute(_args, context) {
        const current = getCycle(context.sessionID);
        if (!current?.failingOutput) throw new Error("Cannot approve RED before observed RED is recorded.");
        setCycle(context.sessionID, { ...current, reviewedRed: true });
        return "RED approved. Minimum production edits are now allowed for this cycle.";
      },
    }),
    rgr_record_proof_of_work_verification: tool({
      description: "Record focused verification after edit-capable implementation work.",
      args: { output: tool.schema.string().describe("Focused verification output after the implementation edit") },
      async execute(args, context) {
        const current = getCycle(context.sessionID);
        if (!current) throw new Error("Cannot record proof-of-work verification before starting an RGR cycle.");
        setCycle(context.sessionID, { ...current, pendingProofOfWork: false });
        recordVerification(context.sessionID, args.output);
        return "Proof-of-work verification recorded. GREEN may be marked if the focused test passed.";
      },
    }),
    rgr_mark_green: tool({
      description: "Mark the active RGR cycle green after the focused test passes.",
      args: { output: tool.schema.string().describe("Passing test output or concise verification summary") },
      async execute(args, context) {
        const current = getCycle(context.sessionID);
        if (!current?.failingOutput) throw new Error("Cannot mark GREEN before observed RED is recorded.");
        if (current.pendingProofOfWork) {
          throw new Error("RGR gate: record explicit proof-of-work verification before marking GREEN.");
        }
        validateRgrGreenVerificationOutput(args.output);
        setCycle(context.sessionID, { ...current, implementationEditToken: false, stage: "green" });
        recordVerification(context.sessionID, args.output);
        return "GREEN recorded. Refactoring is allowed with tests green.";
      },
    }),
    rgr_mark_refactor: tool({
      description: "Mark refactor completion and clear the active RGR cycle.",
      args: { verification: tool.schema.string().describe("Verification run after refactor") },
      async execute(args, context) {
        recordVerification(context.sessionID, args.verification);
        clearCycle(context.sessionID);
        return "REFACTOR recorded. RGR cycle complete. Commit the approved GREEN/refactor state before starting the next RED.";
      },
    }),
    rgr_status: tool({
      description: "Inspect active RGR and verification context.",
      args: {},
      async execute(_args, context) {
        const items = sessionContext(context.sessionID);
        return items.length ? items.join("\n") : "No active RGR cycle recorded for this session.";
      },
    }),
    rgr_recover_implementation_review_veto: tool({
      description: "Record implementation-review veto recovery context before delegating the next RED on a dirty worktree.",
      args: {
        veto: tool.schema.string().describe("Implementation-review veto that requires recovery"),
        nextRedScope: tool.schema.string().describe("Next RED scope allowed for recovery"),
      },
      async execute(args, context) {
        recordImplementationReviewVetoRecovery(context.sessionID, { veto: args.veto, nextRedScope: args.nextRedScope });
        return "Implementation-review veto recovery recorded. rgr-test-author may write the scoped next RED.";
      },
    }),
    adr_create: tool({
      description: "Create a Proposed ADR and store its deferred architecture projection patch.",
      args: {
        title: tool.schema.string().min(1).describe("ADR title"),
        date: tool.schema.string().min(1).describe("ADR date"),
        context: tool.schema.string().min(1).describe("Context motivating the decision"),
        decision: tool.schema.string().min(1).describe("Decision being proposed"),
        consequences: tool.schema.string().min(1).describe("Expected consequences"),
        architecturePatch: tool.schema.object({
          path: tool.schema.string().min(1).describe("Architecture projection path"),
          find: tool.schema.string().min(1).describe("Existing architecture text to replace"),
          replace: tool.schema.string().min(1).describe("Replacement architecture text"),
        }).describe("Paired docs/ARCHITECTURE.md patch"),
        supersedes: tool.schema.array(tool.schema.object({
          path: tool.schema.string().min(1).describe("Superseded ADR path"),
          reason: tool.schema.string().min(1).describe("Reason this ADR supersedes the prior ADR"),
        })).optional().describe("Accepted ADRs superseded by this ADR"),
      },
      async execute(args, context) {
        const title = requireString(args, "title");
        const adrContext = requireString(args, "context");
        const decision = requireString(args, "decision");
        const consequences = requireString(args, "consequences");
        const date = requireString(args, "date");
        const docsDir = "docs";
        const number = nextAdrNumber(docsDir);
        const numberText = String(number).padStart(4, "0");
        const adrPath = path.join(docsDir, `ADR-${numberText}-${titleSlug(title)}.md`);
        const supersedes = Array.isArray(args.supersedes) ? args.supersedes : [];
        const supersedesSection = supersedes.length ? `\n## Supersedes\n\n${supersedes.map((entry) => `- ${requireString(entry, "path")}: ${requireString(entry, "reason")}`).join("\n")}\n` : "";
        const body = `# ADR-${numberText}: ${title}\n\n## Status\n\nProposed\n\n## Date\n\n${date}\n\n## Context\n\n${adrContext}\n\n## Decision\n\n${decision}\n\n## Consequences\n\n${consequences}\n\n${proposedArchitecturePatchSection(args.architecturePatch)}${supersedesSection}`;
        requireAcceptedSupersedes(supersedes);
        fs.writeFileSync(adrPath, body);
        recordTouchedFile(context.sessionID, adrPath);
        return `Created ${adrPath}`;
      },
    }),
    adr_update: tool({
      description: "Update requested sections of a Proposed ADR and store its deferred architecture projection patch.",
      args: {
        path: tool.schema.string().min(1).describe("ADR path"),
        title: tool.schema.string().min(1).describe("ADR title"),
        date: tool.schema.string().min(1).describe("ADR date"),
        context: tool.schema.string().min(1).describe("Context motivating the decision"),
        decision: tool.schema.string().min(1).describe("Decision being proposed"),
        consequences: tool.schema.string().min(1).describe("Expected consequences"),
        sectionsToUpdate: tool.schema.array(tool.schema.string()).describe("ADR sections to rewrite"),
        architecturePatch: tool.schema.object({
          path: tool.schema.string().min(1).describe("Architecture projection path"),
          find: tool.schema.string().min(1).describe("Existing architecture text to replace"),
          replace: tool.schema.string().min(1).describe("Replacement architecture text"),
        }).describe("Paired docs/ARCHITECTURE.md patch"),
        supersedes: tool.schema.array(tool.schema.object({
          path: tool.schema.string().min(1).describe("Superseded ADR path"),
          reason: tool.schema.string().min(1).describe("Reason this ADR supersedes the prior ADR"),
        })).optional().describe("Accepted ADRs superseded by this ADR"),
      },
      async execute(args, context) {
        const adrPath = requireString(args, "path");
        let current = fs.readFileSync(adrPath, "utf8");
        if (adrSectionValue(current, "Status") !== "Proposed") {
          throw new Error("adr_update only updates Proposed ADRs");
        }
        const replacements = {
          title: requireString(args, "title"),
          date: requireString(args, "date"),
          context: requireString(args, "context"),
          decision: requireString(args, "decision"),
          consequences: requireString(args, "consequences"),
        };
        for (const section of args.sectionsToUpdate) {
          const heading = section === "date" ? "Date" : section === "context" ? "Context" : section === "decision" ? "Decision" : section === "consequences" ? "Consequences" : undefined;
          if (heading) current = replaceAdrSection(current, heading, replacements[section]);
        }
        current = replaceProposedArchitecturePatchSection(current, args.architecturePatch);
        const supersedes = Array.isArray(args.supersedes) ? args.supersedes : [];
        if (supersedes.length) {
          current += `\n## Supersedes\n\n${supersedes.map((entry) => `- ${requireString(entry, "path")}: ${requireString(entry, "reason")}`).join("\n")}\n`;
        }
        requireAcceptedSupersedes(supersedes);
        fs.writeFileSync(adrPath, current);
        recordTouchedFile(context.sessionID, adrPath);
        return `Updated ${adrPath}`;
      },
    }),
    adr_accept: tool({
      description: "Accept a Proposed ADR without editing its body sections.",
      args: {
        path: tool.schema.string().min(1).describe("ADR path"),
      },
      async execute(args, context) {
        const adrPath = requireString(args, "path");
        let current = fs.readFileSync(adrPath, "utf8");
        if (adrSectionValue(current, "Status") !== "Proposed") {
          throw new Error("adr_accept only transitions Proposed ADRs");
        }
        requireAcceptedSupersedes(recordedSupersedes(current));
        const storedPatch = storedProposedArchitecturePatch(current);
        if (storedPatch) {
          applyArchitecturePatch(storedPatch);
          current = removeProposedArchitecturePatchSection(current);
          recordTouchedFile(context.sessionID, "docs/ARCHITECTURE.md");
        }
        for (const entry of recordedSupersedes(current)) {
          const superseded = fs.readFileSync(entry.path, "utf8");
          fs.writeFileSync(entry.path, appendSupersededBy(superseded, adrPath, entry.reason));
          recordTouchedFile(context.sessionID, entry.path);
        }
        fs.writeFileSync(adrPath, current.replace("## Status\n\nProposed", "## Status\n\nAccepted"));
        recordTouchedFile(context.sessionID, adrPath);
        return `Accepted ${adrPath}`;
      },
    }),
    adr_reject: tool({
      description: "Reject a Proposed ADR and record the rejection rationale.",
      args: {
        path: tool.schema.string().min(1).describe("ADR path"),
        rationale: tool.schema.string().min(1).describe("Reason the ADR is rejected"),
      },
      async execute(args, context) {
        const adrPath = requireString(args, "path");
        const rationale = requireString(args, "rationale");
        const current = fs.readFileSync(adrPath, "utf8");
        if (adrSectionValue(current, "Status") !== "Proposed") {
          throw new Error("adr_reject only transitions Proposed ADRs");
        }
        fs.writeFileSync(adrPath, replaceAdrSection(current, "Status", `Rejected\n\n## Rejection Rationale\n\n${rationale}`));
        recordTouchedFile(context.sessionID, adrPath);
        return `Rejected ${adrPath}`;
      },
    }),
    adr_delete_unmerged: tool({
      description: "Delete an ADR that has not reached main.",
      args: {
        path: tool.schema.string().min(1).describe("ADR path"),
      },
      async execute(args, context) {
        const adrPath = requireString(args, "path");
        if (!isAdrDocumentPath(adrPath)) {
          throw new Error("adr_delete_unmerged only deletes docs/ADR-*.md paths");
        }
        if (gitPathExistsOnMain(adrPath)) {
          throw new Error(`adr_delete_unmerged refuses to delete ${adrPath} because it exists on main`);
        }
        fs.unlinkSync(adrPath);
        recordTouchedFile(context.sessionID, adrPath);
        cleanAdrReferences(adrPath, context);
        return `Deleted unmerged ADR ${adrPath}`;
      },
    }),
  },
  "tool.execute.before": async (input, output) => {
    if (isEditTool(input.tool)) {
      if (typeof patchTouchesProtectedAdrPath === "function" && patchTouchesProtectedAdrPath(output.args)) {
        throw new Error("ADR path gate: use ADR workflow tools instead of direct edit/write/apply_patch changes for protected architecture decision records.");
      }
      for (const path of changedPathsFromArgs(output.args)) {
        if (typeof isProtectedAdrPath === "function" && isProtectedAdrPath(path)) {
          throw new Error("ADR path gate: use ADR workflow tools instead of direct edit/write/apply_patch changes for protected architecture decision records.");
        }
        recordTouchedFile(input.sessionID, path);
        if (!isProductionRustPath(path) || isLikelyTestPath(path) || isNonBehavioralPath(path)) continue;
        let current = getCycle(input.sessionID);
        if (!current) {
          current = consumeDelegatedImplementationEditLease(input.sessionID);
          if (current) {
            setCycle(input.sessionID, { ...current, implementationEditToken: false, stage: "red" });
          }
        }
        if (!current?.reviewedRed) {
          throw new Error("RGR gate: production Rust edits under crates/*/src require RED review approval recorded with rgr_approve_red.");
        }
        if (isOnMainBranch(worktree)) {
          throw new Error("Branch gate: production Rust edits under crates/*/src require leaving main or calling the explicit override tool.");
        }
        if (current.implementationEditToken) {
          throw new Error("RGR gate: another behavioral production edit requires rerunning the focused command and recording RED or GREEN first.");
        }
        setCycle(input.sessionID, { ...current, implementationEditToken: true });
      }
    }
    if (/todo(write|update)?$/i.test(input.tool) && rejectsWaterfallTodo(output.args)) {
      throw new Error("RGR plan gate: behavior work todo lists must name failing tests, not component-waterfall tasks.");
    }
    if (/^task$/i.test(input.tool) && rejectsBroadDiagnosticTask(output.args)) {
      throw new Error("RGR task gate: rgr-diagnostic-implementer prompts must name one current diagnostic and the allowed immediate change.");
    }
    if (/^task$/i.test(input.tool) && isRgrTestAuthorTask(output.args) && !getCycle(input.sessionID)) {
      if (!consumeImplementationReviewVetoRecovery(input.sessionID)) {
        throw new Error("RGR task gate: start an RGR cycle with rgr_start before delegating to rgr-test-author; recover by starting the cycle or asking the orchestrator to do so.");
      }
    }

    if (/^task$/i.test(input.tool) && isRgrDiagnosticImplementerTask(output.args)) {
      const current = getCycle(input.sessionID);
      if (!current?.reviewedRed) return;
      recordDelegatedImplementationEditLease(delegatedSubagentSessionFromTaskArgs(output.args), input.sessionID, current);
    }
  },
  "tool.execute.after": async (input, output) => {
    if (!/^task$/i.test(input.tool) || !isRgrDiagnosticImplementerTask(output.args)) return;
    const current = getCycle(input.sessionID);
    if (!current) return;
    setCycle(input.sessionID, { ...current, pendingProofOfWork: true });
  },
});

export default AutoReviewDisciplinePlugin;
