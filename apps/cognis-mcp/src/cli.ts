import { spawn } from "child_process";
import * as vscode from "vscode";
import { resolveCliInvocation } from "./binary";
import { modelEnv } from "./model";
import { trace } from "./diagnostics";

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

/** Run the `cognis` CLI surface with repo-root and optional env. */
export async function runCli(
  repoRoot: string,
  args: string[],
  options?: { env?: NodeJS.ProcessEnv; label?: string }
): Promise<CliRunResult> {
  const { command, args: fullArgs } = resolveCliInvocation(repoRoot, args);
  const channel = getOutputChannel();
  const label = options?.label ?? args.join(" ");
  channel.appendLine(`$ ${command} ${fullArgs.join(" ")}`);
  const startedAt = Date.now();
  trace.debug("cli", "spawn", { label, command });

  return new Promise((resolve) => {
    const proc = spawn(command, fullArgs, {
      cwd: repoRoot,
      env: { ...process.env, ...modelEnv(), ...options?.env },
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
      const exitCode = code ?? 1;
      channel.appendLine(`[${label}] exit ${exitCode}`);
      const durationMs = Date.now() - startedAt;
      if (exitCode === 0) {
        trace.info("cli", `${label} ok`, { exitCode, durationMs });
      } else {
        trace.error("cli", `${label} failed`, {
          exitCode,
          durationMs,
          stderrTail: stderr.trim().slice(-400),
        });
      }
      resolve({ exitCode, stdout, stderr });
    });
    proc.on("error", (err) => {
      channel.appendLine(`[${label}] error: ${err.message}`);
      trace.error("cli", `${label} spawn error`, {
        durationMs: Date.now() - startedAt,
        error: err.message,
      });
      resolve({ exitCode: 1, stdout, stderr: `${stderr}\n${err.message}` });
    });
  });
}

export async function runCliJson<T>(
  repoRoot: string,
  args: string[],
  env?: NodeJS.ProcessEnv
): Promise<T> {
  const label = args.join(" ");
  const result = await runCli(repoRoot, args, { env });
  if (result.exitCode !== 0) {
    throw new Error(
      `cognis CLI failed (${result.exitCode}): ${result.stderr || result.stdout}`
    );
  }
  const text = result.stdout.trim();
  const jsonStart = text.indexOf("{");
  const jsonText = jsonStart >= 0 ? text.slice(jsonStart) : text;
  try {
    return JSON.parse(jsonText) as T;
  } catch (err) {
    // A parse failure here is a cross-language contract break (the CLI emitted
    // something the extension can't read). Record it so it is traceable in
    // production instead of surfacing as a vague downstream "undefined".
    trace.error("contract", `${label} returned unparseable JSON`, {
      command: label,
      bytes: jsonText.length,
      head: jsonText.slice(0, 200),
      error: err instanceof Error ? err.message : String(err),
    });
    throw new Error(
      `cognis CLI (${label}) returned output the extension could not parse as JSON. ` +
        "This usually means the backend version does not match the extension. " +
        "See Cognis: Show Diagnostics Log for details."
    );
  }
}
