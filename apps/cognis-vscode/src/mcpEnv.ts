import * as path from "path";

export function expectedDbPathForRepo(repoRoot: string): string {
  return path.resolve(path.join(repoRoot, ".cognis", "uckg.db"));
}

export function envMatchesRepo(
  repoRoot: string,
  env: Record<string, string>
): boolean {
  const envDbPath = env.COGNIS_DB_PATH;
  const resolvedRepo = path.resolve(repoRoot);

  if (!envDbPath || path.resolve(envDbPath) !== expectedDbPathForRepo(repoRoot)) {
    return false;
  }

  const envRoot = env.COGNIS_REPO_ROOT ?? env.REPO_ROOT ?? env.repo_root;
  if (envRoot && path.resolve(envRoot) !== resolvedRepo) {
    return false;
  }
  return true;
}

export function envMatchesExpected(
  actualEnv: Record<string, string>,
  expectedEnv: Record<string, string>
): boolean {
  for (const [key, expectedValue] of Object.entries(expectedEnv)) {
    if (actualEnv[key] !== expectedValue) {
      return false;
    }
  }
  return true;
}
