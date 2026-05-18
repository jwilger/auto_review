import { execFileSync } from "node:child_process";

export type RgrStage =
  | "idle"
  | "red_started"
  | "red_observed"
  | "red_approved"
  | "green_edit_applied"
  | "changed_diagnostic_observed"
  | "changed_diagnostic_approved"
  | "green"
  | "refactor";

export type RgrCycle = {
  behavior: string;
  test: string;
  command?: string;
  failingOutput?: string;
  reviewedRed?: boolean;
  implementationEditToken?: boolean;
  pendingProofOfWork?: boolean;
  currentDiagnostic?: string;
  allowedImmediateChange?: string;
  allowedPaths?: string[];
  stage: RgrStage;
};

const cycles = new Map<string, RgrCycle>();
const delegatedImplementationEditLeases = new Map<string, RgrCycle & { delegatedFromSessionID?: string }>();
const claimableImplementationEditLeases = new Map<string, RgrCycle & { delegatedFromSessionID?: string }>();
const implementationReviewVetoRecovery = new Map<string, { veto: string; nextRedScope: string }>();
const touchedFiles = new Map<string, Set<string>>();
const verification = new Map<string, string>();
const forgejoFeedback = new Map<string, string[]>();

export function normalizePath(path: string): string {
  return path.replaceAll("\\", "/");
}

export function isProductionRustPath(path: string): boolean {
  const normalized = normalizePath(path);
  return /(^|\/)crates\/[^/]+\/src\/.*\.rs$/.test(normalized);
}

export function isLikelyTestPath(path: string): boolean {
  const normalized = normalizePath(path);
  return /(^|\/)(tests|benches)\//.test(normalized) || /(^|\/)crates\/[^/]+\/tests\//.test(normalized);
}

export function isNonBehavioralPath(path: string): boolean {
  const normalized = normalizePath(path);
  return /(^|\/)(docs|deploy)\//.test(normalized) || /(^|\/)README\.md$/.test(normalized) || /(^|\/)CHANGELOG\.md$/.test(normalized) || /\.md$/.test(normalized);
}

export function commandText(args: unknown): string {
  if (!args || typeof args !== "object") return "";
  const record = args as Record<string, unknown>;
  const command = record.command ?? record.cmd ?? record.script;
  return typeof command === "string" ? command : "";
}

export function blocksForgejoInlineReply(command: string): boolean {
  return /\bgh\s+pr\s+comment\b/.test(command) || /\btea\s+comment\s+\d+\b/.test(command) || /\/pulls\/\d+\/comments\b/.test(command);
}

export function blocksUnsafeToolchainCommand(command: string): boolean {
  const checks = [
    /(^|\s)rustup(\s|$)/,
    /(^|\s)git\s+add\s+(-A|-u|\.)(\s|$)/,
    /(^|\s)git\s+commit\s+[^\n]*\s-a(\s|$)/,
    /--no-verify\b/,
    /--no-gpg-sign\b/,
    /(^|\s)git\s+reset\s+--hard\b/,
    /(^|\s)git\s+checkout\s+--\b/,
    /(^|\s)git\s+push\s+[^\n]*--force\b/,
  ];
  return checks.some((check) => check.test(command));
}

export function forgejoInlineReplyPayload(comment: { body: string; path: string; position: number }) {
  return {
    body: comment.body,
    path: comment.path,
    new_position: comment.position,
    old_position: 0,
  };
}

export function validateRgrRedEvidence(output: string): void {
  if (/test result: FAILED\. ([2-9]|\d{2,}) failed;/.test(output)) {
    throw new Error("RED evidence must contain exactly one failing test");
  }
}

export function assertCleanWorktree(worktree: string): void {
  const status = execFileSync("git", ["-C", worktree, "status", "--porcelain"], { encoding: "utf8" });
  if (status.trim()) {
    throw new Error(
      "RGR gate: start a new cycle only from a clean worktree. Commit the approved GREEN/refactor state before starting the next RED."
    );
  }
}

export function setCycle(sessionID: string, cycle: RgrCycle): void {
  cycles.set(sessionID, cycle);
}

export function getCycle(sessionID: string): RgrCycle | undefined {
  return cycles.get(sessionID);
}

export function clearCycle(sessionID: string): void {
  cycles.delete(sessionID);
}

export function recordImplementationReviewVetoRecovery(sessionID: string, recovery: { veto: string; nextRedScope: string }): void {
  implementationReviewVetoRecovery.set(sessionID, recovery);
}

export function consumeImplementationReviewVetoRecovery(sessionID: string): { veto: string; nextRedScope: string } | undefined {
  const recovery = implementationReviewVetoRecovery.get(sessionID);
  implementationReviewVetoRecovery.delete(sessionID);
  return recovery;
}

export function recordTouchedFile(sessionID: string, path: string): void {
  const files = touchedFiles.get(sessionID) ?? new Set<string>();
  files.add(path);
  touchedFiles.set(sessionID, files);
}

export function recordVerification(sessionID: string, status: string): void {
  verification.set(sessionID, status);
}

export function recordDelegatedImplementationEditLease(
  leaseToSessionID: string | undefined,
  leaseFromSessionID: string,
  cycle: RgrCycle,
): string | undefined {
  if (!cycle?.reviewedRed) return;
  const lease = { ...cycle, stage: cycle.stage ?? "red", delegatedFromSessionID: leaseFromSessionID };
  if (leaseToSessionID) {
    delegatedImplementationEditLeases.set(leaseToSessionID, lease);
    return;
  }
  const token = `rgr-lease-${leaseFromSessionID}-${claimableImplementationEditLeases.size + 1}`;
  claimableImplementationEditLeases.set(token, lease);
  return token;
}

export function claimDelegatedImplementationEditLease(token: string, sessionID: string): boolean {
  const lease = claimableImplementationEditLeases.get(token);
  if (!lease) return false;
  claimableImplementationEditLeases.delete(token);
  delegatedImplementationEditLeases.set(sessionID, lease);
  return true;
}

export function consumeDelegatedImplementationEditLease(sessionID: string): RgrCycle | undefined {
  const lease = delegatedImplementationEditLeases.get(sessionID);
  if (lease) {
    delegatedImplementationEditLeases.delete(sessionID);
    if (lease.delegatedFromSessionID) {
      const parentCycle = cycles.get(lease.delegatedFromSessionID);
      if (parentCycle) {
        cycles.set(lease.delegatedFromSessionID, {
          ...parentCycle,
          implementationEditToken: true,
          stage: parentCycle.stage ?? "red",
        });
      }
    }
    return lease;
  }
  return undefined;
}

export function recordForgejoFeedback(sessionID: string, summary: string): void {
  const items = forgejoFeedback.get(sessionID) ?? [];
  items.push(summary);
  forgejoFeedback.set(sessionID, items);
}

export function sessionContext(sessionID: string): string[] {
  const context: string[] = [];
  const cycle = cycles.get(sessionID);
  if (cycle) context.push(`Active RGR cycle: ${JSON.stringify(cycle)}`);
  const recovery = implementationReviewVetoRecovery.get(sessionID);
  if (recovery) context.push(`Implementation-review veto recovery: ${JSON.stringify(recovery)}`);
  const files = touchedFiles.get(sessionID);
  if (files?.size) context.push(`Touched files: ${Array.from(files).sort().join(", ")}`);
  const verify = verification.get(sessionID);
  if (verify) context.push(`Verification status: ${verify}`);
  const feedback = forgejoFeedback.get(sessionID);
  if (feedback?.length) context.push(`Unresolved Forgejo feedback: ${feedback.join("; ")}`);
  return context;
}
