/**
 * Integration-test harness for the Cognis VS Code extension.
 *
 * The production code (`workspace.ts`, `indexd.ts`, `mcpConfig.ts`, `cli.ts`)
 * talks to exactly two external surfaces: the VS Code extension API and child
 * processes spawned via `child_process.spawn` (the `cognis` CLI and the
 * `cognis-indexd` daemon).
 *
 * This harness stubs *only* those two boundaries so integration tests can drive
 * the real orchestration logic — config init, MCP wiring, live-indexing start —
 * against a throwaway temp repo, without needing a Python install or a running
 * VS Code instance. Everything in between (file writes, state transitions,
 * daemon bookkeeping) runs for real.
 */
import { EventEmitter } from "node:events";
import * as fs from "node:fs";
import Module from "node:module";
import * as path from "node:path";

// ---------------------------------------------------------------------------
// VS Code stub
// ---------------------------------------------------------------------------

class FakeEventEmitter<T> {
  private readonly listeners = new Set<(value: T) => void>();

  readonly event = (listener: (value: T) => void) => {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  };

  fire(value: T): void {
    for (const listener of [...this.listeners]) {
      listener(value);
    }
  }

  dispose(): void {
    this.listeners.clear();
  }
}

class FakeOutputChannel {
  readonly lines: string[] = [];
  constructor(public readonly name: string) {}
  append(value: string): void {
    this.lines.push(value);
  }
  appendLine(value: string): void {
    this.lines.push(value);
  }
  show(): void {}
  clear(): void {
    this.lines.length = 0;
  }
  hide(): void {}
  replace(): void {}
  dispose(): void {}
}

interface ConfigSection {
  get<T>(key: string, defaultValue?: T): T | undefined;
  inspect<T>(key: string): { globalValue?: T; workspaceValue?: T; workspaceFolderValue?: T } | undefined;
}

export interface ShownMessage {
  kind: "info" | "warning" | "error";
  message: string;
  items: string[];
}

export interface HarnessState {
  workspaceFolders: Array<{ uri: { fsPath: string }; name: string; index: number }> | undefined;
  appName: string;
  /** section -> key -> value */
  config: Record<string, Record<string, unknown>>;
  /** Queued responses for window.show*Message, consumed in order. */
  messageResponses: string[];
  /** All messages surfaced to the user. */
  shownMessages: ShownMessage[];
  /** Commands executed via vscode.commands.executeCommand. */
  executedCommands: string[];
  outputChannels: FakeOutputChannel[];
}

const harnessState: HarnessState = {
  workspaceFolders: undefined,
  appName: "Cursor",
  config: {},
  messageResponses: [],
  shownMessages: [],
  executedCommands: [],
  outputChannels: [],
};

function makeConfigSection(section: string): ConfigSection {
  return {
    get<T>(key: string, defaultValue?: T): T | undefined {
      const sectionValues = harnessState.config[section];
      if (sectionValues && key in sectionValues) {
        return sectionValues[key] as T;
      }
      return defaultValue;
    },
    inspect<T>(key: string) {
      const sectionValues = harnessState.config[section];
      if (sectionValues && key in sectionValues) {
        return { globalValue: sectionValues[key] as T };
      }
      return undefined;
    },
  };
}

function recordMessage(kind: ShownMessage["kind"], message: string, items: string[]): Promise<string | undefined> {
  harnessState.shownMessages.push({ kind, message, items });
  return Promise.resolve(harnessState.messageResponses.shift());
}

const vscodeStub = {
  workspace: {
    get workspaceFolders() {
      return harnessState.workspaceFolders;
    },
    getConfiguration(section: string): ConfigSection {
      return makeConfigSection(section);
    },
    getWorkspaceFolder(uri: { fsPath: string }) {
      return harnessState.workspaceFolders?.find(
        (folder) => folder.uri.fsPath === uri.fsPath
      );
    },
    openTextDocument(options?: unknown) {
      return Promise.resolve({ options });
    },
    onDidSaveTextDocument: () => ({ dispose() {} }),
    onDidCreateFiles: () => ({ dispose() {} }),
    onDidDeleteFiles: () => ({ dispose() {} }),
    onDidRenameFiles: () => ({ dispose() {} }),
  },
  window: {
    createOutputChannel(name: string) {
      const channel = new FakeOutputChannel(name);
      harnessState.outputChannels.push(channel);
      return channel;
    },
    showInformationMessage(message: string, ...items: string[]) {
      return recordMessage("info", message, items);
    },
    showWarningMessage(message: string, ...items: string[]) {
      return recordMessage("warning", message, items);
    },
    showErrorMessage(message: string, ...items: string[]) {
      return recordMessage("error", message, items);
    },
    showTextDocument() {
      return Promise.resolve(undefined);
    },
    withProgress<T>(_options: unknown, task: (progress: unknown, token: unknown) => Promise<T>) {
      const progress = { report() {} };
      const token = {
        isCancellationRequested: false,
        onCancellationRequested: () => ({ dispose() {} }),
      };
      return task(progress, token);
    },
    createStatusBarItem() {
      return { text: "", tooltip: "", command: "", show() {}, hide() {}, dispose() {} };
    },
    registerWebviewViewProvider: () => ({ dispose() {} }),
  },
  commands: {
    executeCommand(command: string) {
      harnessState.executedCommands.push(command);
      return Promise.resolve(undefined);
    },
    registerCommand: () => ({ dispose() {} }),
  },
  env: {
    get appName() {
      return harnessState.appName;
    },
  },
  Uri: {
    file(fsPath: string) {
      return { fsPath, scheme: "file", path: fsPath };
    },
  },
  EventEmitter: FakeEventEmitter,
  ProgressLocation: { Notification: 15 },
  StatusBarAlignment: { Left: 1, Right: 2 },
};

// ---------------------------------------------------------------------------
// child_process.spawn stub
// ---------------------------------------------------------------------------

export interface SpawnRecord {
  command: string;
  args: string[];
  /** True when this spawn launched the cognis-indexd daemon. */
  isDaemon: boolean;
}

export interface HealthCheck {
  status: "ok" | "warn" | "fail";
  message: string;
}

export interface HealthDescriptor {
  runtime_version: string;
  overall: "ok" | "warn" | "fail";
  checks: Record<string, HealthCheck>;
}

const okCheck: HealthCheck = { status: "ok", message: "ok" };

export const HEALTHY: HealthDescriptor = {
  runtime_version: "0.3.1",
  overall: "ok",
  checks: {
    config: okCheck,
    db: okCheck,
    index: okCheck,
    vector: okCheck,
    embedder: okCheck,
    version: okCheck,
  },
};

/** A fresh repo whose semantic index is still being built in the background. */
export const FRESH_INDEXING: HealthDescriptor = {
  runtime_version: "0.3.1",
  overall: "warn",
  checks: {
    config: okCheck,
    db: okCheck,
    index: { status: "warn", message: "Index is being rebuilt." },
    vector: okCheck,
    embedder: okCheck,
    version: okCheck,
  },
};

interface SpawnBehavior {
  /** Health payload returned by `cognis health --json`. */
  health: HealthDescriptor;
  /** Exit code for `cognis init --quiet`; non-zero simulates a CLI failure. */
  initExitCode: number;
  /** Exit code for `cognis paths` (the Python preflight check). */
  pathsExitCode: number;
  /** When false, `cognis doctor` reports a required prerequisite as missing. */
  prerequisitesReady: boolean;
}

const spawnBehavior: SpawnBehavior = {
  health: HEALTHY,
  initExitCode: 0,
  pathsExitCode: 0,
  prerequisitesReady: true,
};

function generateDoctor(): Record<string, unknown> {
  const ready = spawnBehavior.prerequisitesReady;
  const indexerStatus = ready ? "ok" : "missing";
  return {
    python: "python",
    ready,
    combined_install_target: ready ? "" : ".[indexer]",
    items: [
      {
        id: "indexer",
        label: "Code parsers (tree-sitter)",
        description: "Parses TypeScript, Python, and Go.",
        status: indexerStatus,
        required: true,
        install_target: ".[indexer]",
        detail: ready ? "Installed." : "Not installed: missing tree_sitter",
      },
      {
        id: "mcp",
        label: "MCP server (fastmcp)",
        description: "Serves Cognis tools over MCP.",
        status: "ok",
        required: true,
        install_target: ".[mcp]",
        detail: "Installed.",
      },
    ],
  };
}

const spawnRecords: SpawnRecord[] = [];
/** Fake long-lived daemon processes, so tests can tear them down. */
const liveDaemons = new Set<FakeChildProcess>();

class FakeChildProcess extends EventEmitter {
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  killed = false;
  constructor(public readonly pid: number) {
    super();
  }
  kill(): boolean {
    if (this.killed) {
      return true;
    }
    this.killed = true;
    this.exitCode = 0;
    liveDaemons.delete(this);
    setImmediate(() => this.emit("close", 0));
    return true;
  }
}

let nextPid = 42000;

function readArgValue(args: string[], flag: string): string | undefined {
  const index = args.indexOf(flag);
  if (index >= 0 && index + 1 < args.length) {
    return args[index + 1];
  }
  return undefined;
}

function generatePaths(repoRoot: string): Record<string, unknown> {
  const cognisDir = path.join(repoRoot, ".cognis");
  return {
    repo_root: repoRoot,
    cognis_dir: cognisDir,
    config_path: path.join(cognisDir, "config.yaml"),
    db_path: path.join(cognisDir, "uckg.db"),
    indexd_status_path: path.join(cognisDir, "indexd-status.json"),
    audit_log_path: path.join(cognisDir, "audit.log"),
    capsule_cache_dir: path.join(cognisDir, "capsule_cache"),
    golden_set_path: path.join(cognisDir, "eval", "golden.jsonl"),
    runtime_version: spawnBehavior.health.runtime_version,
    commands: {
      python: "python",
      cognis_cli: null,
      cognis_mcpd: null,
      cognis_indexd: null,
      cognis_cli_module: "cognis.cli.main",
      cognis_mcpd_module: "cognis_mcpd.main",
      cognis_indexd_module: "cognis_indexd.main",
    },
  };
}

function generateMcpConfig(repoRoot: string, args: string[]): Record<string, unknown> {
  const host = readArgValue(args, "--host") ?? "cursor";
  const serverName = readArgValue(args, "--server-name") ?? "cognis-repo";
  const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
  const env = { COGNIS_DB_PATH: dbPath };
  return {
    host,
    format: "mcpServers",
    repo_root: repoRoot,
    server_name: serverName,
    config: {
      mcpServers: {
        [serverName]: {
          command: "python",
          args: ["-m", "cognis_mcpd.main"],
          env,
        },
      },
    },
    config_paths: {},
    env,
  };
}

function finishCli(proc: FakeChildProcess, stdout: string, exitCode: number): void {
  setImmediate(() => {
    if (stdout) {
      proc.stdout.emit("data", Buffer.from(stdout));
    }
    proc.exitCode = exitCode;
    proc.emit("close", exitCode);
  });
}

function fakeSpawn(command: string, args: string[]): FakeChildProcess {
  const proc = new FakeChildProcess(nextPid++);
  const repoRoot = readArgValue(args, "--repo-root") ?? process.cwd();
  const isDaemon = args.includes("cognis_indexd.main");
  spawnRecords.push({ command, args: [...args], isDaemon });

  if (isDaemon) {
    // The daemon stays alive until the test (or production code) kills it.
    liveDaemons.add(proc);
    return proc;
  }

  // Everything else is a one-shot `cognis` CLI invocation.
  if (args.includes("mcp-config")) {
    finishCli(proc, JSON.stringify(generateMcpConfig(repoRoot, args)), 0);
  } else if (args.includes("doctor")) {
    finishCli(proc, JSON.stringify(generateDoctor()), 0);
  } else if (args.includes("health")) {
    finishCli(proc, JSON.stringify(spawnBehavior.health), 0);
  } else if (args.includes("paths")) {
    finishCli(proc, JSON.stringify(generatePaths(repoRoot)), spawnBehavior.pathsExitCode);
  } else if (args.includes("init")) {
    // Mirror the real `cognis init`: a successful run materializes
    // `.cognis/config.yaml`, which is what `isWorkspaceConfigured` keys off of.
    // Reproducing that here is essential — the live-indexing start path is
    // gated on the config existing, so a stub that skipped it would hide the
    // very fresh-user indexing bug these tests guard against.
    if (spawnBehavior.initExitCode === 0) {
      const cognisDir = path.join(repoRoot, ".cognis");
      fs.mkdirSync(cognisDir, { recursive: true });
      fs.writeFileSync(
        path.join(cognisDir, "config.yaml"),
        "version: 1\n",
        "utf8"
      );
    }
    finishCli(proc, "", spawnBehavior.initExitCode);
  } else if (args.includes("index")) {
    finishCli(proc, "", 0);
  } else {
    finishCli(proc, "", 0);
  }
  return proc;
}

// ---------------------------------------------------------------------------
// Installation + lifecycle
// ---------------------------------------------------------------------------

let installed = false;

/** Install the `vscode` module hook and the `child_process.spawn` stub once. */
export function installHarness(): void {
  if (installed) {
    return;
  }
  installed = true;

  const moduleApi = Module as unknown as {
    _load: (request: string, parent: unknown, isMain: boolean) => unknown;
  };
  const originalLoad = moduleApi._load;
  moduleApi._load = function (request: string, parent: unknown, isMain: boolean): unknown {
    if (request === "vscode") {
      return vscodeStub;
    }
    return originalLoad.call(this, request, parent, isMain);
  };

  // Patch the cached child_process module so destructured `spawn` imports in
  // the production code resolve to our stub. The compiled CommonJS reads
  // `child_process_N.spawn(...)` at call time off the cached module object, so
  // replacing the export is enough. Patch both the bare and `node:`-prefixed
  // ids since they share one exports object but may be required either way.
  for (const id of ["child_process", "node:child_process"]) {
    const childProcess = require(id) as { spawn: unknown };
    childProcess.spawn = fakeSpawn as unknown as typeof childProcess.spawn;
  }
}

export interface ConfigureOptions {
  health?: HealthDescriptor;
  initExitCode?: number;
  pathsExitCode?: number;
  appName?: string;
  config?: Record<string, Record<string, unknown>>;
  /** When false, `cognis doctor` reports a required prerequisite as missing. */
  prerequisitesReady?: boolean;
}

/**
 * Reset harness state and point it at a workspace folder. Call at the start of
 * every test so state never leaks between cases.
 */
export function resetHarness(repoRoot: string, options: ConfigureOptions = {}): void {
  harnessState.workspaceFolders = [
    { uri: { fsPath: repoRoot }, name: path.basename(repoRoot), index: 0 },
  ];
  harnessState.appName = options.appName ?? "Cursor";
  harnessState.messageResponses = [];
  harnessState.shownMessages = [];
  harnessState.executedCommands = [];
  harnessState.outputChannels = [];
  harnessState.config = {
    cognis: {
      pythonPath: "",
      // Keep all MCP writes inside the temp repo so tests never touch $HOME.
      mcpHost: "cursor",
      mcpConfigScope: "workspace",
      mcpWarmSemanticOnStartup: true,
      ...(options.config?.cognis ?? {}),
    },
    python: {
      defaultInterpreterPath: "",
      ...(options.config?.python ?? {}),
    },
  };

  spawnBehavior.health = options.health ?? HEALTHY;
  spawnBehavior.initExitCode = options.initExitCode ?? 0;
  spawnBehavior.pathsExitCode = options.pathsExitCode ?? 0;
  spawnBehavior.prerequisitesReady = options.prerequisitesReady ?? true;
  spawnRecords.length = 0;
}

export function getSpawnRecords(): SpawnRecord[] {
  return [...spawnRecords];
}

export function getDaemonSpawns(): SpawnRecord[] {
  return spawnRecords.filter((record) => record.isDaemon);
}

export function getHarnessState(): HarnessState {
  return harnessState;
}

/** Tear down any fake daemon processes left running after a test. */
export function killLiveDaemons(): void {
  for (const proc of [...liveDaemons]) {
    proc.kill();
  }
}

/** A noop progress reporter for driving setup flows directly. */
export function silentProgress(): { report: (value: { message?: string }) => void; messages: string[] } {
  const messages: string[] = [];
  return {
    report(value: { message?: string }) {
      if (value.message) {
        messages.push(value.message);
      }
    },
    messages,
  };
}

/** A cancellation token that is never cancelled. */
export function noCancelToken(): { isCancellationRequested: boolean; onCancellationRequested: () => { dispose: () => void } } {
  return {
    isCancellationRequested: false,
    onCancellationRequested: () => ({ dispose() {} }),
  };
}

// Self-install on import. The `vscode` require-hook must be in place *before*
// any production module (workspace.ts, indexd.ts, …) is required, so a test
// only needs to `import "./testHarness"` ahead of the modules under test.
installHarness();
