import * as fs from "fs";
import * as path from "path";
import { getOutputChannel } from "./cli";

/**
 * Keep the per-repo ``.cognis/`` runtime directory out of version control.
 *
 * ``.cognis/`` holds the UCKG database, capsule cache, audit log, and live
 * status file — machine-specific, regeneratable artifacts that should never be
 * committed. After setup, when the workspace is a git repo and the entry is
 * missing, Cognis adds it to ``.gitignore`` automatically (idempotent) so a
 * fresh user never accidentally commits it.
 */

const COGNIS_IGNORE_ENTRY = ".cognis/";

/** Return the workspace ``.gitignore`` path. */
function gitignorePath(repoRoot: string): string {
  return path.join(repoRoot, ".gitignore");
}

/** True only when *repoRoot* is (or is inside) a git working tree. */
export function isGitRepository(repoRoot: string): boolean {
  let current = path.resolve(repoRoot);
  // Walk up a few levels: the workspace may be a subdirectory of the repo.
  for (let depth = 0; depth < 6; depth += 1) {
    if (fs.existsSync(path.join(current, ".git"))) {
      return true;
    }
    const parent = path.dirname(current);
    if (parent === current) {
      break;
    }
    current = parent;
  }
  return false;
}

/**
 * Return true when ``.cognis/`` is already ignored by the workspace
 * ``.gitignore``. Matches the common spellings (``.cognis``, ``.cognis/``,
 * ``/.cognis``) so we don't nag when an equivalent entry exists.
 */
export function isCognisIgnored(repoRoot: string): boolean {
  const file = gitignorePath(repoRoot);
  if (!fs.existsSync(file)) {
    return false;
  }
  let content: string;
  try {
    content = fs.readFileSync(file, "utf8");
  } catch {
    return false;
  }
  return content
    .split(/\r?\n/)
    .map((line) => line.trim())
    .some((line) => {
      if (!line || line.startsWith("#")) {
        return false;
      }
      const normalized = line.replace(/^\/+/, "").replace(/\/+$/, "");
      return normalized === ".cognis";
    });
}

/**
 * Whether Cognis should add ``.cognis/`` to ``.gitignore``: only when the
 * workspace is a git repo and the entry is not already present.
 */
export function shouldRemindGitignore(repoRoot: string): boolean {
  return isGitRepository(repoRoot) && !isCognisIgnored(repoRoot);
}

/**
 * Append ``.cognis/`` to the workspace ``.gitignore`` (creating it if needed).
 * Idempotent: re-running when the entry exists is a no-op. Returns the path on
 * success, or ``undefined`` on I/O failure (logged, never thrown).
 */
export function addCognisToGitignore(repoRoot: string): string | undefined {
  const file = gitignorePath(repoRoot);
  try {
    if (isCognisIgnored(repoRoot)) {
      return file;
    }
    let prefix = "";
    if (fs.existsSync(file)) {
      const existing = fs.readFileSync(file, "utf8");
      if (existing.length > 0 && !existing.endsWith("\n")) {
        prefix = "\n";
      }
      // Add a blank separator line when appending to non-empty content.
      if (existing.trim().length > 0) {
        prefix += "\n";
      }
    }
    const block = `${prefix}# Cognis per-repo runtime state (index DB, caches, audit log)\n${COGNIS_IGNORE_ENTRY}\n`;
    fs.appendFileSync(file, block, "utf8");
    return file;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    getOutputChannel().appendLine(`[gitignore] failed to update ${file}: ${message}`);
    return undefined;
  }
}
