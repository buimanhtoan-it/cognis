import * as path from "path";

export function expectedDbPathForRepo(repoRoot: string): string {
  return path.resolve(path.join(repoRoot, ".cognis", "uckg.db"));
}

/** Normalize paths for equality checks (drive letter + slash style on Windows). */
export function normalizePathForCompare(filePath: string): string {
  return path.resolve(filePath).replace(/\\/g, "/").toLowerCase();
}

export function pathsEqual(a: string, b: string): boolean {
  return normalizePathForCompare(a) === normalizePathForCompare(b);
}

const PATH_ENV_KEYS = new Set([
  "COGNIS_DB_PATH",
  "COGNIS_REPO_ROOT",
  "REPO_ROOT",
  "repo_root",
]);

export function envMatchesRepo(
  repoRoot: string,
  env: Record<string, string>
): boolean {
  const envDbPath = env.COGNIS_DB_PATH;
  const resolvedRepo = path.resolve(repoRoot);

  if (!envDbPath || !pathsEqual(envDbPath, expectedDbPathForRepo(repoRoot))) {
    return false;
  }

  const envRoot = env.COGNIS_REPO_ROOT ?? env.REPO_ROOT ?? env.repo_root;
  if (envRoot && !pathsEqual(envRoot, resolvedRepo)) {
    return false;
  }
  return true;
}

export function envMatchesExpected(
  actualEnv: Record<string, string>,
  expectedEnv: Record<string, string>
): boolean {
  for (const [key, expectedValue] of Object.entries(expectedEnv)) {
    const actualValue = actualEnv[key];
    if (PATH_ENV_KEYS.has(key)) {
      if (!actualValue || !pathsEqual(actualValue, expectedValue)) {
        return false;
      }
      continue;
    }
    if (actualValue !== expectedValue) {
      return false;
    }
  }
  return true;
}
