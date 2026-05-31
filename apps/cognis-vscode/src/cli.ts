import { spawn } from "child_process";
import * as vscode from "vscode";
import { resolvePythonExecutable } from "./python";

const CLI_MODULE = "cognis.cli.main";
let outputChannel: vscode.OutputChannel | undefined;

export interface CliRunResult {
  exitCode: number;
  stdout: string;
  stderr: string;
}

export function getOutputChannel(): vscode.OutputChannel {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel("Cognis");
  }
  return outputChannel;
}

/** Run `python -m cognis.cli.main` with repo-root and optional env. */
export async function runCli(
  repoRoot: string,
  args: string[],
  options?: { env?: NodeJS.ProcessEnv; label?: string }
): Promise<CliRunResult> {
  const python = resolvePythonExecutable();
  const channel = getOutputChannel();
  const fullArgs = [
    "-m",
    CLI_MODULE,
    "--repo-root",
    repoRoot,
    ...args,
  ];
  const label = options?.label ?? args.join(" ");
  channel.appendLine(`$ ${python} ${fullArgs.join(" ")}`);

  return new Promise((resolve) => {
    const proc = spawn(python, fullArgs, {
      cwd: repoRoot,
      env: { ...process.env, ...options?.env },
    });
    let stdout = "";
    let stderr = "";
    proc.stdout.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stdout += text;
      channel.append(text);
    });
    proc.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stderr += text;
      channel.append(text);
    });
    proc.on("close", (code) => {
      channel.appendLine(`[${label}] exit ${code ?? 1}`);
      resolve({ exitCode: code ?? 1, stdout, stderr });
    });
    proc.on("error", (err) => {
      channel.appendLine(`[${label}] error: ${err.message}`);
      resolve({ exitCode: 1, stdout, stderr: `${stderr}\n${err.message}` });
    });
  });
}

export async function runCliJson<T>(
  repoRoot: string,
  args: string[],
  env?: NodeJS.ProcessEnv
): Promise<T> {
  const result = await runCli(repoRoot, args, { env });
  if (result.exitCode !== 0) {
    throw new Error(
      `cognis CLI failed (${result.exitCode}): ${result.stderr || result.stdout}`
    );
  }
  const text = result.stdout.trim();
  const jsonStart = text.indexOf("{");
  const jsonText = jsonStart >= 0 ? text.slice(jsonStart) : text;
  return JSON.parse(jsonText) as T;
}
